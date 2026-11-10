// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{
    Control, DownloadedUpdate, Error, FileInstall, InstallContext, InstallCustody, ReceiptBinding,
    custody::{BACKUP_CAP, CustodyReceipt, RECORD_CAP, create_record, hex, read_record},
    helper::{
        HANDSHAKE, HelperResult, HelperState, Lock, Request, acquire_lease, lease_path, lock_path,
        transaction_path,
    },
    install::{OwnedFile, hash_file, reject_links},
    model::check_control,
    protocol::{decode_digest, sha256},
};

/// Required acknowledgement that the outer host exits without business replay.
#[derive(Clone, Copy, Debug)]
pub enum ExitAcknowledgement {
    /// Keep the outer lifetime guard alive through diagnostics, then exit.
    ExitImmediatelyAndRetainRecovery,
}

/// Outer-main-owned lifetime capability for one queued helper.
///
/// Create this in the outermost `main` frame and borrow it through every update
/// call. It must outlive all output/error handling after helper readiness. TCP
/// EOF is capability release, not proof of OS death; Windows replacement still
/// retries sharing failures under a finite deadline. No PID is consulted.
#[derive(Default)]
pub struct ParentLifetime {
    active: Option<ActiveHelper>,
}

struct ActiveHelper {
    stream: TcpStream,
    child: Child,
}

impl ParentLifetime {
    /// Construct without filesystem, process, network or global-state effects.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this outer frame still owns a helper lifetime connection.
    pub fn is_pending(&self) -> bool {
        self.active.is_some()
    }

    /// Request cancellation before capability release without dropping the guard.
    ///
    /// A failed send is ambiguous and requires reconciliation, not an assertion
    /// that installation was prevented. The helper treats any byte as abort.
    pub fn cancel(&mut self) -> Result<(), Error> {
        if let Some(active) = self.active.as_mut() {
            active
                .stream
                .set_write_timeout(Some(Duration::from_millis(100)))
                .map_err(|_| Error::RecoveryRequired)?;
            active
                .stream
                .write_all(&[1])
                .map_err(|_| Error::RecoveryRequired)?;
        }
        Ok(())
    }
}

impl Drop for ParentLifetime {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            // Close only our socket/child handles. Do not kill an authenticated
            // ready helper: it owns the finite post-release transaction.
            drop(active.stream);
            drop(active.child);
        }
    }
}

/// Queued work only; independent helper verification precedes this receipt.
#[derive(Debug, Serialize)]
pub struct PendingUpdate {
    /// Always `PendingParentExit`; never `Applied` on spawn.
    pub state: HelperState,
    /// Always false in the parent.
    pub installed: bool,
    /// Always true; the outer host must not dispatch another business command.
    pub exit_required: bool,
    /// Signed candidate version, not yet installed.
    pub version: String,
    /// Exact nonce-owned recovery directory.
    pub transaction: PathBuf,
}

/// Prepares signed bytes and queues the separately installed trusted helper.
///
/// The installation directory remains exclusively installer-owned. This is not
/// an adversarial same-user filesystem-race defense, an OS-signature verifier or
/// permission to launch a downloaded helper.
pub struct RunningInstaller {
    installation: FileInstall,
}

impl RunningInstaller {
    /// Require exact current executable/digest/nonce and explicit exit agreement.
    pub fn new(installation: FileInstall, _exit: ExitAcknowledgement) -> Self {
        Self { installation }
    }

    /// Prepare, independently verify in the installed helper, then report pending.
    ///
    /// This deliberately does not implement `Installer`: queuing is not a
    /// successful `UpdateEngine::install` or installed-byte receipt. Once a child
    /// connects, `lifetime` retains it even if later readiness/output fails.
    pub fn queue(
        &mut self,
        update: &DownloadedUpdate,
        context: InstallContext,
        control: &dyn Control,
        lifetime: &mut ParentLifetime,
    ) -> Result<PendingUpdate, Error> {
        if context.custody != InstallCustody::Standalone {
            return Err(Error::Custody);
        }
        update.require_major_policy(context.allow_major)?;
        if context.budget_ms == 0 || context.budget_ms > 120_000 || lifetime.is_pending() {
            return Err(Error::InvalidLimits);
        }
        let start = control.snapshot().monotonic_ms;
        let deadline = start
            .checked_add(context.budget_ms)
            .ok_or(Error::Deadline)?;
        let checkpoint = || check_control(control, start, deadline).map(|_| ());
        checkpoint()?;
        reject_links(&self.installation.path)?;
        let path = fs::canonicalize(&self.installation.path).map_err(|_| Error::Custody)?;
        let running = std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|_| Error::Custody)?;
        if path != running {
            return Err(Error::RunningExecutable);
        }
        let manifest = update.manifest().manifest();
        let receipt_path = path.with_file_name(".fab-rs-install.json");
        let receipt = CustodyReceipt::load(
            &receipt_path,
            &path,
            ReceiptBinding {
                repository: &manifest.repository,
                version: update.installed_version(),
                target: crate::Target::parse(&manifest.target)?,
                variant: &manifest.variant,
            },
            control,
        )?;
        if decode_digest(&receipt.sha256)? != self.installation.expected_current_sha256 {
            return Err(Error::Custody);
        }
        if receipt.backups.len() >= BACKUP_CAP {
            return Err(Error::Retention);
        }
        let snapshot = read_record(&receipt_path, RECORD_CAP)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| Error::Custody)?;
        if !metadata.is_file() || metadata.permissions().readonly() {
            return Err(Error::Custody);
        }
        let nonce = hex(&self.installation.nonce);
        let transaction = transaction_path(&path, &nonce);
        let request = Request {
            schema: 1,
            nonce: nonce.clone(),
            executable: path.clone(),
            transaction: transaction.clone(),
            receipt_sha256: hex(&sha256(&snapshot)),
            tag: manifest.tag.clone(),
            origin: format!(
                "https://github.com/{}/releases/tag/{}",
                manifest.repository,
                crate::model::encode_leaf(&manifest.tag)
            ),
            allow_major: context.allow_major,
        };
        let request_bytes = serde_json::to_vec(&request).map_err(|_| Error::Schema)?;
        let lock = Lock {
            schema: 1,
            nonce,
            transaction: transaction.clone(),
            request_sha256: hex(&sha256(&request_bytes)),
        };
        let lock_bytes = serde_json::to_vec(&lock).map_err(|_| Error::Schema)?;
        if request_bytes.len() > RECORD_CAP || lock_bytes.len() > RECORD_CAP {
            return Err(Error::Limit);
        }
        let lock_path = lock_path(&path);
        create_record(&lock_path, &lock_bytes).map_err(|error| {
            if error == Error::InstallConflict {
                Error::Lock
            } else {
                error
            }
        })?;
        let mut prepared = Preparation {
            lock_path,
            lock_bytes,
            transaction: transaction.clone(),
            lease_path: lease_path(&path, &lock.nonce),
            created_directory: false,
            lease_created: false,
            nonce: self.installation.nonce,
            keep: false,
        };
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        builder
            .create(&transaction)
            .map_err(|_| Error::InstallConflict)?;
        prepared.created_directory = true;
        let mut lease_options = OpenOptions::new();
        lease_options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lease_options.mode(0o600);
        }
        let lease_file = lease_options
            .open(&prepared.lease_path)
            .map_err(|_| Error::InstallConflict)?;
        let mut lease = OwnedFile {
            path: prepared.lease_path.clone(),
            file: Some(lease_file),
            keep: false,
            cleanup_nonce: None,
        };
        let lease_file = lease.file.as_mut().ok_or(Error::Io)?;
        lease_file
            .write_all(&self.installation.nonce)
            .map_err(|_| Error::Io)?;
        lease_file.sync_all().map_err(|_| Error::Io)?;
        lease.file.take();
        lease.keep = true;
        prepared.lease_created = true;
        checkpoint()?;
        create_record(&transaction.join("receipt.previous.json"), &snapshot)?;
        let (signed, signature) = update.signed_metadata();
        create_record(&transaction.join("manifest.json"), signed)?;
        create_record(&transaction.join("manifest.sig"), signature)?;
        create_record(&transaction.join("request.json"), &request_bytes)?;
        let stage = transaction.join("asset.bin");
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&stage).map_err(|_| Error::InstallConflict)?;
        for chunk in update.bytes().chunks(64 * 1024) {
            checkpoint()?;
            file.write_all(chunk).map_err(|_| Error::Io)?;
        }
        file.set_permissions(metadata.permissions())
            .map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        drop(file);
        if hash_file(&stage, update.bytes().len(), &checkpoint)? != update.manifest().digest {
            return Err(Error::Integrity);
        }
        if control.snapshot().unix_seconds >= manifest.expires_at {
            return Err(Error::Expired);
        }
        checkpoint()?;
        // Once spawn begins the helper may have opened recovery files. Preserve
        // evidence on every ambiguity rather than cleanup behind a live child.
        prepared.keep = true;
        launch(&receipt.helper, &request, control, lifetime)?;
        Ok(PendingUpdate {
            state: HelperState::PendingParentExit,
            installed: false,
            exit_required: true,
            version: manifest.version.clone(),
            transaction,
        })
    }
}

struct Preparation {
    lock_path: PathBuf,
    lock_bytes: Vec<u8>,
    transaction: PathBuf,
    lease_path: PathBuf,
    created_directory: bool,
    lease_created: bool,
    nonce: [u8; 32],
    keep: bool,
}

impl Drop for Preparation {
    fn drop(&mut self) {
        if self.keep
            || read_record(&self.lock_path, RECORD_CAP).ok().as_deref()
                != Some(self.lock_bytes.as_slice())
        {
            return;
        }
        if self.created_directory {
            for name in [
                "receipt.previous.json",
                "manifest.json",
                "manifest.sig",
                "request.json",
                "asset.bin",
            ] {
                let _ = fs::remove_file(self.transaction.join(name));
            }
            if self.lease_created {
                if read_record(&self.lease_path, 33).ok().as_deref() != Some(self.nonce.as_slice())
                {
                    return;
                }
                if fs::remove_file(&self.lease_path).is_err() {
                    return;
                }
            }
            if fs::remove_dir(&self.transaction).is_err() {
                return;
            }
        }
        let _ = fs::remove_file(&self.lock_path);
    }
}

fn helper_command(helper: &Path, executable: &Path) -> Result<Command, Error> {
    let root = executable.parent().ok_or(Error::Custody)?;
    crate::custody::absolute(helper)?;
    reject_links(helper)?;
    reject_links(root)?;
    let mut command = Command::new(helper);
    command
        .env_clear()
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        let receipt_path = executable.with_file_name(".fab-rs-install.json");
        let bytes = match fs::symlink_metadata(&receipt_path) {
            Ok(_) => read_record(&receipt_path, RECORD_CAP)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let lock: Lock =
                    serde_json::from_slice(&read_record(&lock_path(executable), RECORD_CAP)?)
                        .map_err(|_| Error::Lock)?;
                if lock.transaction != transaction_path(executable, &lock.nonce) {
                    return Err(Error::Lock);
                }
                read_record(&lock.transaction.join("receipt.previous.json"), RECORD_CAP)?
            }
            Err(_) => return Err(Error::Custody),
        };
        let receipt: CustodyReceipt = serde_json::from_slice(&bytes).map_err(|_| Error::Custody)?;
        if receipt.executable != executable || receipt.helper != helper {
            return Err(Error::Custody);
        }
        let system = receipt
            .windows_system_root
            .as_deref()
            .ok_or(Error::NativeBoundary)?;
        let canonical_system = CustodyReceipt::validate_system_root(system)?;
        command.env("SystemRoot", canonical_system);
    }
    Ok(command)
}

fn launch(
    helper: &Path,
    request: &Request,
    control: &dyn Control,
    lifetime: &mut ParentLifetime,
) -> Result<(), Error> {
    let expected_nonce = decode_digest(&request.nonce)?;
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| Error::NativeBoundary)?;
    listener.set_nonblocking(true).map_err(|_| Error::Io)?;
    let address = listener.local_addr().map_err(|_| Error::Io)?;
    let mut child = helper_command(helper, &request.executable)?
        .arg("--apply")
        .arg(request.transaction.join("request.json"))
        .arg(&request.nonce)
        .arg(address.to_string())
        .spawn()
        .map_err(|_| Error::RecoveryRequired)?;
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => {
                lifetime.active = Some(ActiveHelper { stream, child });
                let active = lifetime.active.as_mut().ok_or(Error::RecoveryRequired)?;
                let nonce = match receive_nonce(&mut active.stream, started, control) {
                    Ok(nonce) => nonce,
                    Err(error) => {
                        if error == Error::Cancelled {
                            lifetime.cancel()?;
                        }
                        return Err(Error::RecoveryRequired);
                    }
                };
                if nonce != expected_nonce {
                    return Err(Error::RecoveryRequired);
                }
                // This post-ready checkpoint must not drop the outer lifetime.
                if control.snapshot().cancelled {
                    lifetime.cancel()?;
                    return Err(Error::RecoveryRequired);
                }
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {
                terminate_unready(&mut child);
                return Err(Error::RecoveryRequired);
            }
        }
        if started.elapsed() >= HANDSHAKE
            || control.snapshot().cancelled
            || child
                .try_wait()
                .map_err(|_| Error::RecoveryRequired)?
                .is_some()
        {
            terminate_unready(&mut child);
            return Err(Error::RecoveryRequired);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn receive_nonce(
    stream: &mut TcpStream,
    started: Instant,
    control: &dyn Control,
) -> Result<[u8; 32], Error> {
    let mut nonce = [0; 32];
    let mut received = 0;
    while received < nonce.len() {
        if control.snapshot().cancelled {
            return Err(Error::Cancelled);
        }
        let remaining = HANDSHAKE.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(Error::Deadline);
        }
        stream
            .set_read_timeout(Some(remaining.min(Duration::from_millis(100))))
            .map_err(|_| Error::Io)?;
        match stream.read(&mut nonce[received..]) {
            Ok(0) => return Err(Error::NativeBoundary),
            Ok(count) => received += count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(Error::NativeBoundary),
        }
    }
    Ok(nonce)
}

fn terminate_unready(child: &mut Child) {
    let _ = child.kill();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Reconcile only the exact independently verified transaction beside this image.
///
/// No lock means `None`. An active kernel-locked helper returns `HelperPending`.
/// Unknown/legacy/foreign locks are never reclaimed. The helper repairs only its
/// exact old-displaced or binary/receipt-commit window, not arbitrary stale data.
pub fn reconcile(
    executable: &Path,
    helper: &Path,
    expected_helper_digest: [u8; 32],
) -> Result<Option<HelperResult>, Error> {
    crate::custody::absolute(executable)?;
    crate::custody::absolute(helper)?;
    let root = executable.parent().ok_or(Error::Custody)?;
    reject_links(root)?;
    reject_links(helper)?;
    let verification_started = Instant::now();
    let checkpoint = || {
        if verification_started.elapsed() >= Duration::from_secs(30) {
            Err(Error::Deadline)
        } else {
            Ok(())
        }
    };
    if helper != executable.with_file_name(crate::helper_name())
        || hash_file(helper, crate::custody::FILE_CAP, &checkpoint)? != expected_helper_digest
    {
        return Err(Error::Custody);
    }
    let path = lock_path(executable);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Lock),
        Ok(_) => {}
    }
    let lock: Lock =
        serde_json::from_slice(&read_record(&path, RECORD_CAP)?).map_err(|_| Error::Lock)?;
    if lock.schema != 1
        || decode_digest(&lock.nonce)? == [0; 32]
        || lock.transaction != transaction_path(executable, &lock.nonce)
    {
        return Err(Error::Lock);
    }
    let file = acquire_lease(executable, &lock.nonce)?;
    drop(file);
    let mut child = helper_command(helper, executable)?
        .arg("--reconcile")
        .arg(lock.transaction.join("request.json"))
        .arg(&lock.nonce)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| Error::RecoveryRequired)?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| Error::RecoveryRequired)? {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(35) {
            terminate_unready(&mut child);
            return Err(Error::RecoveryRequired);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Err(Error::RecoveryRequired);
    }
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or(Error::RecoveryRequired)?
        .take(RECORD_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::RecoveryRequired)?;
    if bytes.len() > RECORD_CAP {
        return Err(Error::Limit);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::RecoveryRequired)
}
