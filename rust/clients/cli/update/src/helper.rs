// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    Channel, Control, Error, Identity, Limits, Repository, Snapshot, Target, Trust,
    VerificationContext,
    custody::{
        BACKUP_CAP, CustodyReceipt, FILE_CAP, RECORD_CAP, RollbackImage, absolute, create_record,
        hex, read_record, rollback_path,
    },
    install::{hash_file, reject_links, sibling},
    protocol::{decode_digest, sha256, validate_binary, verify_manifest},
};

pub(crate) const HANDSHAKE: Duration = Duration::from_secs(5);
const EXIT_BUDGET: Duration = Duration::from_secs(30);
const MUTATION_BUDGET: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(20);

/// Ordered durable transaction facts; spawning alone is never `Applied`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HelperState {
    /// Exact signed bytes and custody snapshot have been staged.
    PreparedVerified,
    /// Independently verified helper is waiting for lifetime-capability release.
    PendingParentExit,
    /// Previous executable was moved; recovery must restore a runnable path.
    OldDisplaced,
    /// New executable bytes are committed; custody receipt may still be old.
    Applied,
    /// New version/digest are committed to the installer receipt.
    InstallerReceiptCommitted,
    /// Exact-owned transient state was reconciled; verified rollback is retained.
    Reconciled,
}

/// Truthful terminal local result, including receipt-commit recovery debt.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HelperResult {
    /// Last completed local lifecycle phase.
    pub state: HelperState,
    /// Whether the new executable bytes are actually present.
    pub installed: bool,
    /// Whether the new installer version/digest were committed.
    pub receipt_committed: bool,
    /// Whether exact-owned recovery evidence must be retained.
    pub recovery_required: bool,
    /// Signed candidate version; not necessarily the installed version.
    pub version: String,
    /// Exact version/digest/nonce-owned retained previous image.
    pub rollback: PathBuf,
}

/// Helper-owned publisher/channel/target/variant pins, never read from requests.
pub struct HelperPolicy {
    trust: Trust,
    channel: Channel,
    target: Target,
    variant: String,
}

impl HelperPolicy {
    /// Inject independent policy for a trusted host or offline fixture binary.
    ///
    /// The production binary uses only `production`; request arguments never
    /// configure this value. The installer must protect the helper executable.
    pub fn new(
        trust: Trust,
        channel: Channel,
        target: Target,
        variant: &str,
    ) -> Result<Self, Error> {
        crate::model::validate_leaf(variant)?;
        if variant.len() > 64 {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            trust,
            channel,
            target,
            variant: variant.to_owned(),
        })
    }

    /// Read build-time pins only; absent key/variant fails before filesystem I/O.
    pub fn production() -> Result<Self, Error> {
        let value = option_env!("FAB_RELEASE_PUBLIC_KEY").ok_or(Error::TrustRequired)?;
        let key = decode_digest(&value.to_ascii_lowercase()).map_err(|_| Error::TrustRequired)?;
        let variant = option_env!("FAB_RELEASE_VARIANT").ok_or(Error::TrustRequired)?;
        let channel = match option_env!("FAB_RELEASE_CHANNEL").unwrap_or("stable") {
            "stable" => Channel::Stable,
            "preview" => Channel::Preview,
            _ => return Err(Error::TrustRequired),
        };
        Self::new(
            Trust::new(Repository::new("microsoft", "fabric-cli")?, Some(key))?,
            channel,
            native_target()?,
            variant,
        )
    }
}

/// Determine only this compiled native target, without PATH or environment input.
pub(crate) fn native_target() -> Result<Target, Error> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(Target::WindowsX64),
        ("windows", "aarch64") => Ok(Target::WindowsArm64),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Ok(Target::LinuxX64),
        ("linux", "aarch64") if cfg!(target_env = "gnu") => Ok(Target::LinuxArm64),
        ("macos", "x86_64") => Ok(Target::MacX64),
        ("macos", "aarch64") => Ok(Target::MacArm64),
        _ => Err(Error::UnsupportedTarget),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) schema: u32,
    pub(crate) nonce: String,
    pub(crate) executable: PathBuf,
    pub(crate) transaction: PathBuf,
    pub(crate) receipt_sha256: String,
    pub(crate) tag: String,
    pub(crate) origin: String,
    pub(crate) allow_major: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lock {
    pub(crate) schema: u32,
    pub(crate) nonce: String,
    pub(crate) transaction: PathBuf,
    pub(crate) request_sha256: String,
}

pub(crate) fn transaction_path(executable: &Path, nonce: &str) -> PathBuf {
    sibling(executable, &format!(".update-{nonce}"))
}

pub(crate) fn lock_path(executable: &Path) -> PathBuf {
    sibling(executable, ".update-lock")
}

pub(crate) fn lease_path(executable: &Path, nonce: &str) -> PathBuf {
    sibling(executable, &format!(".update-lease-{nonce}"))
}

pub(crate) fn acquire_lease(executable: &Path, nonce: &str) -> Result<File, Error> {
    let path = lease_path(executable, nonce);
    reject_links(&path)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| Error::Lock)?;
    if !metadata.is_file() || metadata.len() != 32 {
        return Err(Error::Lock);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| Error::Lock)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => Error::HelperPending,
        _ => Error::Lock,
    })?;
    // Windows byte-range locks deny reads through a second file handle, even in
    // the same process. Read the lease through the handle that owns its lock.
    let mut bytes = Vec::new();
    file.rewind().map_err(|_| Error::Lock)?;
    (&mut file)
        .take(33)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Lock)?;
    if bytes != decode_digest(nonce)? {
        return Err(Error::Lock);
    }
    Ok(file)
}

struct Budget {
    started: Instant,
}

impl Budget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    fn check(&self) -> Result<(), Error> {
        if self.started.elapsed() >= Duration::from_secs(90) {
            Err(Error::Deadline)
        } else {
            Ok(())
        }
    }
}

impl Control for Budget {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |v| v.as_secs()),
            monotonic_ms: self
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            cancelled: false,
        }
    }
}

struct Transaction {
    request: Request,
    lock: Lock,
    _lease_file: File,
    previous: CustodyReceipt,
    next: CustodyReceipt,
    manifest: crate::VerifiedManifest,
    root: PathBuf,
    budget: Budget,
}

impl Transaction {
    fn open(policy: &HelperPolicy, path: &Path, nonce: &str) -> Result<Self, Error> {
        let budget = Budget::new();
        absolute(path)?;
        let request_bytes = read_record(path, RECORD_CAP)?;
        let request: Request = serde_json::from_slice(&request_bytes).map_err(|_| Error::Schema)?;
        absolute(&request.executable)?;
        absolute(&request.transaction)?;
        let parent = request.executable.parent().ok_or(Error::Custody)?;
        reject_links(parent)?;
        if request.schema != 1
            || request.nonce != nonce
            || decode_digest(nonce)? == [0; 32]
            || request.transaction != transaction_path(&request.executable, nonce)
            || path != request.transaction.join("request.json")
            || fs::canonicalize(parent).map_err(|_| Error::Custody)? != parent
        {
            return Err(Error::Binding);
        }
        let lock_path = lock_path(&request.executable);
        let lock_bytes = read_record(&lock_path, RECORD_CAP)?;
        let lock: Lock = serde_json::from_slice(&lock_bytes).map_err(|_| Error::Lock)?;
        if lock.schema != 1
            || lock.nonce != nonce
            || lock.transaction != request.transaction
            || lock.request_sha256 != hex(&sha256(&request_bytes))
        {
            return Err(Error::Lock);
        }
        let lease_file = acquire_lease(&request.executable, nonce)?;
        let snapshot = read_record(
            &request.transaction.join("receipt.previous.json"),
            RECORD_CAP,
        )?;
        if sha256(&snapshot) != decode_digest(&request.receipt_sha256)? {
            return Err(Error::Custody);
        }
        let previous: CustodyReceipt =
            serde_json::from_slice(&snapshot).map_err(|_| Error::Custody)?;
        previous.validate_shape(
            &request.executable,
            &policy.trust.repository.full_name(),
            policy.target,
            &policy.variant,
        )?;
        previous.verify_components(&|| budget.check())?;
        let own = std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|_| Error::Custody)?;
        reject_links(&own)?;
        if own != previous.helper
            || hash_file(&own, FILE_CAP, &|| budget.check())?
                != decode_digest(&previous.helper_sha256)?
        {
            return Err(Error::Custody);
        }
        if previous.backups.len() >= BACKUP_CAP {
            return Err(Error::Retention);
        }
        let identity = Identity::new(
            &previous.version,
            policy.channel,
            policy.target,
            &policy.variant,
        )?;
        let manifest_bytes = read_record(
            &request.transaction.join("manifest.json"),
            Limits::default().manifest_bytes,
        )?;
        let signature = read_record(&request.transaction.join("manifest.sig"), 64)?;
        let candidate = crate::model::tag_version(&request.tag)?;
        let manifest = verify_manifest(
            &manifest_bytes,
            &signature,
            VerificationContext {
                trust: &policy.trust,
                identity: &identity,
                tag: &request.tag,
                prerelease: !candidate.pre.is_empty(),
                now: budget.snapshot().unix_seconds,
                limits: Limits::default(),
            },
        )?;
        if request.origin != policy.trust.repository.release_page(&request.tag)
            || (candidate.major != identity.current.major && !request.allow_major)
        {
            return Err(Error::Binding);
        }
        let rollback = RollbackImage {
            path: rollback_path(
                &request.executable,
                &previous.version,
                &previous.sha256,
                nonce,
            ),
            version: previous.version.clone(),
            sha256: previous.sha256.clone(),
            nonce: nonce.to_owned(),
        };
        let mut next = previous.clone();
        next.version = manifest.manifest.version.clone();
        next.sha256 = manifest.manifest.sha256.clone();
        next.backups.push(rollback);
        if serde_json::to_vec(&next).map_err(|_| Error::Schema)?.len() > RECORD_CAP {
            return Err(Error::Limit);
        }
        let result = Self {
            root: request.transaction.clone(),
            request,
            lock,
            _lease_file: lease_file,
            previous,
            next,
            manifest,
            budget,
        };
        result.check_lock()?;
        if result.root.join("asset.bin").exists()
            || result.matches(&result.request.executable, &result.next.sha256)?
        {
            result.check_stage_or_committed()?;
        } else if result.root.join("reconciled.json").exists()
            && result.matches(&result.request.executable, &result.previous.sha256)?
        {
            result.check_owned_names()?;
        } else {
            return Err(Error::Integrity);
        }
        result.check_receipt()?;
        Ok(result)
    }

    fn check_lock(&self) -> Result<(), Error> {
        self.budget.check()?;
        let bytes = read_record(&lock_path(&self.request.executable), RECORD_CAP)?;
        let actual: Lock = serde_json::from_slice(&bytes).map_err(|_| Error::Lock)?;
        if actual != self.lock {
            return Err(Error::Lock);
        }
        reject_links(&self.root)?;
        reject_links(self.request.executable.parent().ok_or(Error::Custody)?)?;
        Ok(())
    }

    fn mark(&self, phase: HelperState) -> Result<(), Error> {
        self.check_lock()?;
        let name = phase_name(phase);
        let bytes = serde_json::to_vec(&(self.lock.clone(), phase)).map_err(|_| Error::Schema)?;
        let path = self.root.join(name);
        if path.exists() {
            if read_record(&path, RECORD_CAP)? != bytes {
                return Err(Error::InstallConflict);
            }
            return Ok(());
        }
        create_record(&path, &bytes)
    }

    fn check_receipt(&self) -> Result<(), Error> {
        let path = self
            .request
            .executable
            .with_file_name(".fab-rs-install.json");
        let current = match fs::symlink_metadata(&path) {
            Ok(_) => {
                let bytes = read_record(&path, RECORD_CAP)?;
                Some(serde_json::from_slice::<CustodyReceipt>(&bytes).map_err(|_| Error::Custody)?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(Error::Custody),
        };
        if current.as_ref() != Some(&self.previous) && current.as_ref() != Some(&self.next) {
            // Only our exact saved receipt may explain the rename/receipt gap.
            if current.is_some()
                || read_record(&self.root.join("receipt.displaced.json"), RECORD_CAP)?
                    != read_record(&self.root.join("receipt.previous.json"), RECORD_CAP)?
            {
                return Err(Error::Custody);
            }
        }
        Ok(())
    }

    fn matches(&self, path: &Path, digest: &str) -> Result<bool, Error> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => {
                reject_links(path)?;
                Ok(hash_file(path, FILE_CAP, &|| self.budget.check())? == decode_digest(digest)?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            _ => Err(Error::InstallConflict),
        }
    }

    fn check_stage_or_committed(&self) -> Result<(), Error> {
        let stage = self.root.join("asset.bin");
        let candidate = if stage.exists() {
            stage
        } else {
            self.request.executable.clone()
        };
        if !self.matches(&candidate, &self.next.sha256)?
            || fs::metadata(&candidate).map_err(|_| Error::Io)?.len()
                != self.manifest.manifest.bytes
        {
            return Err(Error::Integrity);
        }
        let mut header = Vec::new();
        File::open(candidate)
            .map_err(|_| Error::Io)?
            .take(64 * 1024)
            .read_to_end(&mut header)
            .map_err(|_| Error::Io)?;
        validate_binary(&header, Target::parse(&self.next.target)?)
    }

    fn wait_parent(&self, address: SocketAddr) -> Result<(), Error> {
        if !address.ip().is_loopback() || !address.is_ipv4() {
            return Err(Error::NativeBoundary);
        }
        let mut stream =
            TcpStream::connect_timeout(&address, HANDSHAKE).map_err(|_| Error::NativeBoundary)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|_| Error::Io)?;
        self.mark(HelperState::PendingParentExit)?;
        let nonce = decode_digest(&self.request.nonce)?;
        let sending = Instant::now();
        let mut sent = 0;
        while sent < nonce.len() {
            self.check_lock()?;
            let remaining = HANDSHAKE.saturating_sub(sending.elapsed());
            if remaining.is_zero() {
                return Err(Error::Deadline);
            }
            stream
                .set_write_timeout(Some(remaining))
                .map_err(|_| Error::Io)?;
            match stream.write(&nonce[sent..]) {
                Ok(0) => return Err(Error::NativeBoundary),
                Ok(count) => sent += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(Error::NativeBoundary),
            }
        }
        let started = Instant::now();
        let mut byte = [0; 1];
        loop {
            self.check_lock()?;
            if started.elapsed() >= EXIT_BUDGET {
                return Err(Error::Deadline);
            }
            match stream.read(&mut byte) {
                Ok(0) => return Ok(()),
                Ok(_) => return Err(Error::Cancelled),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => return Err(Error::NativeBoundary),
            }
        }
    }

    fn rollback(&self) -> &RollbackImage {
        &self.next.backups[self.next.backups.len() - 1]
    }

    fn apply(&self, hook: &mut dyn FnMut(HelperState) -> Result<(), Error>) -> Result<(), Error> {
        self.check_lock()?;
        self.check_receipt()?;
        self.check_stage_or_committed()?;
        if !self.matches(&self.request.executable, &self.previous.sha256)? {
            return Err(Error::InstallConflict);
        }
        let snapshot = self.budget.snapshot();
        if snapshot.unix_seconds >= self.manifest.manifest.expires_at {
            return Err(Error::Expired);
        }
        let rollback = &self.rollback().path;
        fs::hard_link(&self.request.executable, rollback).map_err(|_| Error::InstallConflict)?;
        if !self.matches(rollback, &self.previous.sha256)? {
            return Err(Error::Integrity);
        }
        self.check_lock()?;
        let started = Instant::now();
        let displaced = self.root.join("previous.bin");
        // EOF releases a lifetime capability, not an OS process handle. On
        // Windows the old image can remain locked briefly until process exit.
        loop {
            self.check_lock()?;
            if self.budget.snapshot().unix_seconds >= self.manifest.manifest.expires_at {
                return Err(Error::Expired);
            }
            if !self.matches(&self.request.executable, &self.previous.sha256)? {
                return Err(Error::InstallConflict);
            }
            match fs::rename(&self.request.executable, &displaced) {
                Ok(()) => break,
                Err(e)
                    if cfg!(windows)
                        && (e.kind() == std::io::ErrorKind::PermissionDenied
                            || matches!(e.raw_os_error(), Some(32 | 33)))
                        && started.elapsed() < MUTATION_BUDGET =>
                {
                    std::thread::sleep(TICK);
                }
                Err(_) => return Err(Error::Install),
            }
        }
        self.mark(HelperState::OldDisplaced)
            .map_err(|_| Error::RecoveryRequired)?;
        let replace = hook(HelperState::OldDisplaced).and_then(|()| {
            self.check_lock()?;
            self.check_stage_or_committed()?;
            if self.budget.snapshot().unix_seconds >= self.manifest.manifest.expires_at {
                return Err(Error::Expired);
            }
            fs::rename(self.root.join("asset.bin"), &self.request.executable)
                .map_err(|_| Error::Install)
        });
        if replace.is_err() {
            // A normal failure restores the exact previous path. Abrupt helper
            // death retains previous.bin for the next bounded reconciliation.
            self.restore_previous()
                .map_err(|_| Error::RecoveryRequired)?;
            return Err(Error::RecoveryRequired);
        }
        self.mark(HelperState::Applied)
            .map_err(|_| Error::RecoveryRequired)?;
        hook(HelperState::Applied).map_err(|_| Error::RecoveryRequired)?;
        Ok(())
    }

    fn restore_previous(&self) -> Result<(), Error> {
        self.check_lock()?;
        let displaced = self.root.join("previous.bin");
        match fs::symlink_metadata(&self.request.executable) {
            Ok(_) => return Err(Error::InstallConflict),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Custody),
        }
        if !self.matches(&displaced, &self.previous.sha256)? {
            return Err(Error::InstallConflict);
        }
        fs::rename(displaced, &self.request.executable).map_err(|_| Error::Io)
    }

    fn commit_receipt(&self) -> Result<(), Error> {
        self.check_lock()?;
        if !self.matches(&self.request.executable, &self.next.sha256)?
            || !self.matches(&self.rollback().path, &self.previous.sha256)?
        {
            return Err(Error::RecoveryRequired);
        }
        self.check_receipt()?;
        let receipt = self
            .request
            .executable
            .with_file_name(".fab-rs-install.json");
        if receipt.exists() {
            let bytes = read_record(&receipt, RECORD_CAP)?;
            let current: CustodyReceipt =
                serde_json::from_slice(&bytes).map_err(|_| Error::Custody)?;
            if current == self.next {
                return self.mark(HelperState::InstallerReceiptCommitted);
            }
        }
        let next_path = self.root.join("receipt.next.json");
        let bytes = serde_json::to_vec(&self.next).map_err(|_| Error::Schema)?;
        if next_path.exists() {
            if read_record(&next_path, RECORD_CAP)? != bytes {
                return Err(Error::InstallConflict);
            }
        } else {
            create_record(&next_path, &bytes)?;
        }
        let displaced = self.root.join("receipt.displaced.json");
        if receipt.exists() {
            fs::rename(&receipt, &displaced).map_err(|_| Error::RecoveryRequired)?;
        }
        fs::rename(next_path, receipt).map_err(|_| Error::RecoveryRequired)?;
        self.mark(HelperState::InstallerReceiptCommitted)
    }

    fn finish(&self, installed: bool) -> Result<HelperResult, Error> {
        self.check_lock()?;
        self.check_receipt()?;
        self.check_owned_names()?;
        let expected = if installed {
            &self.next
        } else {
            &self.previous
        };
        if !self.matches(&self.request.executable, &expected.sha256)? {
            return Err(Error::RecoveryRequired);
        }
        let receipt = read_record(
            &self
                .request
                .executable
                .with_file_name(".fab-rs-install.json"),
            RECORD_CAP,
        )?;
        let actual: CustodyReceipt =
            serde_json::from_slice(&receipt).map_err(|_| Error::Custody)?;
        if actual != *expected {
            return Err(Error::RecoveryRequired);
        }
        for (name, digest) in [
            ("previous.bin", self.previous.sha256.as_str()),
            ("asset.bin", self.next.sha256.as_str()),
        ] {
            let path = self.root.join(name);
            if path.exists() {
                if !self.matches(&path, digest)? {
                    return Err(Error::RecoveryRequired);
                }
                fs::remove_file(path).map_err(|_| Error::RecoveryRequired)?;
            }
        }
        for name in ["receipt.displaced.json", "receipt.next.json"] {
            let path = self.root.join(name);
            if path.exists() {
                let bytes = read_record(&path, RECORD_CAP)?;
                let value: CustodyReceipt =
                    serde_json::from_slice(&bytes).map_err(|_| Error::Custody)?;
                if value != self.previous && value != self.next {
                    return Err(Error::RecoveryRequired);
                }
                fs::remove_file(path).map_err(|_| Error::RecoveryRequired)?;
            }
        }
        if !installed && self.rollback().path.exists() {
            if !self.matches(&self.rollback().path, &self.previous.sha256)? {
                return Err(Error::RecoveryRequired);
            }
            fs::remove_file(&self.rollback().path).map_err(|_| Error::RecoveryRequired)?;
        }
        self.mark(HelperState::Reconciled)?;
        let result = HelperResult {
            state: HelperState::Reconciled,
            installed,
            receipt_committed: installed,
            recovery_required: false,
            version: self.next.version.clone(),
            rollback: self.rollback().path.clone(),
        };
        let result_bytes = serde_json::to_vec(&result).map_err(|_| Error::Schema)?;
        if self.root.join("result.json").exists() {
            if read_record(&self.root.join("result.json"), RECORD_CAP)? != result_bytes {
                return Err(Error::RecoveryRequired);
            }
        } else {
            create_record(&self.root.join("result.json"), &result_bytes)?;
        }
        self.check_lock()?;
        Ok(result)
    }

    fn check_owned_names(&self) -> Result<(), Error> {
        const OWNED: &[&str] = &[
            "request.json",
            "manifest.json",
            "manifest.sig",
            "receipt.previous.json",
            "asset.bin",
            "previous.bin",
            "receipt.displaced.json",
            "receipt.next.json",
            "prepared.json",
            "pending.json",
            "displaced.json",
            "applied.json",
            "receipt-committed.json",
            "reconciled.json",
            "result.json",
        ];
        for (index, entry) in fs::read_dir(&self.root).map_err(|_| Error::Io)?.enumerate() {
            if index >= OWNED.len() {
                return Err(Error::Limit);
            }
            let entry = entry.map_err(|_| Error::Io)?;
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| OWNED.contains(&name))
            {
                return Err(Error::InstallConflict);
            }
            reject_links(&entry.path())?;
            if !entry.file_type().map_err(|_| Error::Io)?.is_file() {
                return Err(Error::InstallConflict);
            }
        }
        for phase in [
            HelperState::PreparedVerified,
            HelperState::PendingParentExit,
            HelperState::OldDisplaced,
            HelperState::Applied,
            HelperState::InstallerReceiptCommitted,
            HelperState::Reconciled,
        ] {
            let path = self.root.join(phase_name(phase));
            if path.exists()
                && read_record(&path, RECORD_CAP)?
                    != serde_json::to_vec(&(self.lock.clone(), phase)).map_err(|_| Error::Schema)?
            {
                return Err(Error::InstallConflict);
            }
        }
        Ok(())
    }
}

fn phase_name(phase: HelperState) -> &'static str {
    match phase {
        HelperState::PreparedVerified => "prepared.json",
        HelperState::PendingParentExit => "pending.json",
        HelperState::OldDisplaced => "displaced.json",
        HelperState::Applied => "applied.json",
        HelperState::InstallerReceiptCommitted => "receipt-committed.json",
        HelperState::Reconciled => "reconciled.json",
    }
}

/// Execute the installed helper's strict argument protocol with independent pins.
///
/// Accepted arguments are `--apply REQUEST NONCE LOOPBACK_ADDRESS` or
/// `--reconcile REQUEST NONCE`. There are no keys, URLs, credentials or program
/// paths accepted as authority. This function never exits the caller process.
pub fn run_helper(policy: HelperPolicy, arguments: &[OsString]) -> Result<HelperResult, Error> {
    run_helper_inner(policy, arguments, &mut |_| Ok(()))
}

/// Offline fixture-only phase injection; production entry never calls this API.
#[cfg(feature = "fixture-harness")]
pub fn run_helper_with_fault(
    policy: HelperPolicy,
    arguments: &[OsString],
    hook: &mut dyn FnMut(HelperState) -> Result<(), Error>,
) -> Result<HelperResult, Error> {
    run_helper_inner(policy, arguments, hook)
}

fn run_helper_inner(
    policy: HelperPolicy,
    arguments: &[OsString],
    hook: &mut dyn FnMut(HelperState) -> Result<(), Error>,
) -> Result<HelperResult, Error> {
    let mode = arguments
        .first()
        .and_then(|v| v.to_str())
        .ok_or(Error::Schema)?;
    if !matches!((mode, arguments.len()), ("--apply", 4) | ("--reconcile", 3)) {
        return Err(Error::Schema);
    }
    let nonce = arguments[2].to_str().ok_or(Error::Schema)?;
    let transaction = Transaction::open(&policy, Path::new(&arguments[1]), nonce)?;
    transaction.mark(HelperState::PreparedVerified)?;
    if mode == "--apply" {
        let address: SocketAddr = arguments[3]
            .to_str()
            .ok_or(Error::Schema)?
            .parse()
            .map_err(|_| Error::NativeBoundary)?;
        transaction.wait_parent(address)?;
        transaction.apply(hook)?;
        transaction
            .commit_receipt()
            .map_err(|_| Error::RecoveryRequired)?;
    } else if transaction.matches(&transaction.request.executable, &transaction.next.sha256)? {
        transaction
            .commit_receipt()
            .map_err(|_| Error::RecoveryRequired)?;
    } else {
        if !transaction.request.executable.exists() {
            transaction
                .restore_previous()
                .map_err(|_| Error::RecoveryRequired)?;
        }
        if !transaction.matches(
            &transaction.request.executable,
            &transaction.previous.sha256,
        )? {
            return Err(Error::RecoveryRequired);
        }
    }
    let installed =
        transaction.matches(&transaction.request.executable, &transaction.next.sha256)?;
    let result = transaction
        .finish(installed)
        .map_err(|_| Error::RecoveryRequired)?;
    let lock = transaction.lock.clone();
    let path = lock_path(&transaction.request.executable);
    let lease = lease_path(&transaction.request.executable, &lock.nonce);
    let actual: Lock =
        serde_json::from_slice(&read_record(&path, RECORD_CAP)?).map_err(|_| Error::Lock)?;
    if actual != lock {
        return Err(Error::Lock);
    }
    // Enumerate only fixed protocol-owned names, never directory contents.
    // Unknown files make remove_dir fail; they are not deletion candidates.
    for name in [
        "request.json",
        "manifest.json",
        "manifest.sig",
        "receipt.previous.json",
        "prepared.json",
        "pending.json",
        "displaced.json",
        "applied.json",
        "receipt-committed.json",
        "reconciled.json",
        "result.json",
    ] {
        match fs::remove_file(lock.transaction.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::RecoveryRequired),
        }
    }
    fs::remove_dir(&lock.transaction).map_err(|_| Error::RecoveryRequired)?;
    // Keep the independent lease held until the readable nonce lock is removed.
    // Its separate sibling path does not hold the transaction directory open.
    fs::remove_file(path).map_err(|_| Error::RecoveryRequired)?;
    drop(transaction);
    if read_record(&lease, 33)? != decode_digest(&lock.nonce)? {
        return Err(Error::Lock);
    }
    fs::remove_file(lease).map_err(|_| Error::RecoveryRequired)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_keeps_nonce_record_readable_and_blocks_other_helpers() {
        let root = std::env::var_os("FAB_UPDATE_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        assert!(root.is_absolute());
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = root.join(format!("helper-lease-{:x}-{unique:x}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let executable = root.join("fixture.exe");
        let nonce = hex(&[7; 32]);
        let lock = lock_path(&executable);
        let lease = lease_path(&executable, &nonce);
        create_record(&lock, b"independently-readable-nonce-record").unwrap();
        create_record(&lease, &[7; 32]).unwrap();
        let held = acquire_lease(&executable, &nonce).unwrap();
        assert_eq!(
            read_record(&lock, RECORD_CAP).unwrap(),
            b"independently-readable-nonce-record"
        );
        assert!(matches!(
            acquire_lease(&executable, &nonce),
            Err(Error::HelperPending)
        ));
        drop(held);
        let reacquired = acquire_lease(&executable, &nonce).unwrap();
        drop(reacquired);
        assert_eq!(read_record(&lease, 33).unwrap(), [7; 32]);
        fs::remove_file(lease).unwrap();
        fs::remove_file(lock).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
