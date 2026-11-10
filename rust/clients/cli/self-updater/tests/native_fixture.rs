// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual local parent/helper lifecycle; no network, credentials or production key.

#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]
#![cfg(feature = "fixture-harness")]

use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fabric_cli_update::{CustodyReceipt, Error, HelperState, helper_name, reconcile};
use ring::digest;
use serde_json::{Value, json};

mod support;

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.0.kill();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(1) {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn hash(path: &Path) -> [u8; 32] {
    let bytes = fs::read(path).unwrap();
    let mut result = [0; 32];
    result.copy_from_slice(digest::digest(&digest::SHA256, &bytes).as_ref());
    result
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn wait_until(budget: Duration, mut predicate: impl FnMut() -> bool) {
    let started = Instant::now();
    while !predicate() {
        assert!(started.elapsed() < budget, "local fixture deadline elapsed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_child(child: &mut OwnedChild) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "parent exceeded fixed deadline"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Fixture {
    root: PathBuf,
    executable: PathBuf,
    helper: PathBuf,
    helper_digest: [u8; 32],
    original: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = support::test_root().join(format!(
            "bounded-helper-{:x}-{unique:x}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let executable = root.join(if cfg!(windows) {
            "parent.exe"
        } else {
            "parent"
        });
        let helper = root.join(helper_name());
        fs::copy(
            env!("CARGO_BIN_EXE_fabric-update-lifecycle-fixture"),
            &executable,
        )
        .unwrap();
        fs::copy(env!("CARGO_BIN_EXE_fabric-update-helper-fixture"), &helper).unwrap();
        fs::write(root.join(".fixture-authorized"), b"offline-only").unwrap();
        let original = hash(&executable);
        let helper_digest = hash(&helper);
        let target = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
            ("windows", "aarch64") => "aarch64-pc-windows-msvc",
            ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
            ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "aarch64-apple-darwin",
            _ => panic!("unsupported fixture target"),
        };
        let receipt = json!({
            "schema":2, "product":"fab-rs",
            "repository":"fixture-publisher/fixture-product", "custody":"standalone",
            "executable":executable, "version":"1.0.0", "target":target,
            "variant":"local-native-fixture", "sha256":hex(&original),
            "helper":helper, "helper_sha256":hex(&helper_digest), "backups":[],
            "windows_system_root":if cfg!(windows) { Some(fs::canonicalize(std::env::var_os("SystemRoot").expect("Windows runtime root")).unwrap()) } else {None}
        });
        write_private(
            &root.join(".fab-rs-install.json"),
            &serde_json::to_vec(&receipt).unwrap(),
        );
        Self {
            root,
            executable,
            helper,
            helper_digest,
            original,
        }
    }

    fn receipt(&self) -> CustodyReceipt {
        serde_json::from_slice(&fs::read(self.root.join(".fab-rs-install.json")).unwrap()).unwrap()
    }

    #[cfg(windows)]
    fn use_plain_system_root(&self) -> (PathBuf, PathBuf) {
        use std::path::{Component, Prefix};
        let mut receipt = self.receipt();
        let canonical = fs::canonicalize(receipt.windows_system_root.as_ref().unwrap()).unwrap();
        let mut components = canonical.components();
        let drive = match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
                _ => panic!("fixture runtime must be local"),
            },
            _ => panic!("fixture runtime requires a drive prefix"),
        };
        assert_eq!(components.next(), Some(Component::RootDir));
        let mut plain = PathBuf::from(format!("{}:\\", char::from(drive)));
        for component in components {
            let Component::Normal(value) = component else {
                panic!("unexpected canonical runtime component");
            };
            plain.push(value);
        }
        receipt.windows_system_root = Some(plain.clone());
        write_private(
            &self.root.join(".fab-rs-install.json"),
            &serde_json::to_vec(&receipt).unwrap(),
        );
        (plain, canonical)
    }

    fn start(&self, version: &str, hold_ms: u64, mode: &str) -> OwnedChild {
        let ready = self.root.join(".fixture-parent-ready.json");
        if ready.exists() {
            fs::remove_file(ready).unwrap();
        }
        let mut command = Command::new(&self.executable);
        command.env_clear();
        #[cfg(windows)]
        command.env(
            "SystemRoot",
            self.receipt().windows_system_root.as_ref().unwrap(),
        );
        OwnedChild(
            command
                .args([version, &hold_ms.to_string(), mode])
                .current_dir(&self.root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    fs::File::create(self.root.join(".fixture-parent.stderr.log")).unwrap(),
                ))
                .spawn()
                .unwrap(),
        )
    }

    fn ready(&self, child: &mut OwnedChild) -> PathBuf {
        let marker = self.root.join(".fixture-parent-ready.json");
        let mut pending = None;
        wait_until(Duration::from_secs(10), || {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "parent failed before helper readiness: {}; diagnostic: {}",
                self.root.display(),
                fs::read_to_string(self.root.join(".fixture-parent.stderr.log"))
                    .unwrap_or_default()
            );
            pending = fs::read(&marker)
                .ok()
                .and_then(|v| serde_json::from_slice::<Value>(&v).ok());
            pending.is_some()
        });
        let pending = pending.unwrap();
        assert_eq!(pending["state"], "pending_parent_exit");
        assert_eq!(pending["installed"], false);
        assert_eq!(pending["exit_required"], true);
        PathBuf::from(pending["transaction"].as_str().unwrap())
    }

    fn lock(&self) -> PathBuf {
        let name = self.executable.file_name().unwrap().to_string_lossy();
        self.root.join(format!(".{name}.update-lock"))
    }

    fn lease(&self) -> PathBuf {
        let lock: Value = serde_json::from_slice(&fs::read(self.lock()).unwrap()).unwrap();
        let name = self.executable.file_name().unwrap().to_string_lossy();
        self.root.join(format!(
            ".{name}.update-lease-{}",
            lock["nonce"].as_str().unwrap()
        ))
    }

    fn complete(&self, version: &str) {
        wait_until(Duration::from_secs(15), || {
            fs::read(self.root.join(".fab-rs-install.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<CustodyReceipt>(&bytes).ok())
                .is_some_and(|receipt| receipt.version == version && !self.lock().exists())
        });
        let receipt = self.receipt();
        assert_eq!(receipt.sha256, hex(&hash(&self.executable)));
        assert_eq!(receipt.helper_sha256, hex(&self.helper_digest));
        assert_eq!(hash(&self.helper), self.helper_digest);
    }

    fn inactive_lock(&self) {
        wait_until(Duration::from_secs(10), || {
            let Ok(file) = OpenOptions::new().read(true).write(true).open(self.lease()) else {
                return false;
            };
            file.try_lock().is_ok()
        });
    }

    fn fault(&self, value: &str) {
        fs::write(self.root.join(".fixture-fault"), value).unwrap();
    }

    fn clear_fault(&self) {
        fs::remove_file(self.root.join(".fixture-fault")).unwrap();
    }
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn two_successive_upgrades_advance_receipt_and_reconcile_owned_state() {
    let fixture = Fixture::new();
    #[cfg(windows)]
    let (plain_runtime, canonical_runtime) = fixture.use_plain_system_root();
    let foreign = fixture.root.join(".foreign-rollback");
    fs::write(&foreign, b"foreign-stale-image").unwrap();
    let mut previous = fixture.original;
    for (index, version) in ["1.1.0", "1.2.0"].iter().enumerate() {
        let mut child = fixture.start(version, 600, "normal");
        let transaction = fixture.ready(&mut child);
        assert_eq!(hash(&fixture.executable), previous);
        assert_eq!(fixture.receipt().backups.len(), index);
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(wait_child(&mut child).success());
        fixture.complete(version);
        wait_until(Duration::from_secs(5), || !transaction.exists());
        let receipt = fixture.receipt();
        #[cfg(windows)]
        {
            assert_eq!(receipt.windows_system_root.as_ref(), Some(&plain_runtime));
            let environment: Value = serde_json::from_slice(
                &fs::read(fixture.root.join(".fixture-helper-runtime.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                PathBuf::from(environment["system_root"].as_str().unwrap()),
                canonical_runtime
            );
        }
        assert_eq!(receipt.backups.len(), index + 1);
        assert_eq!(hash(&receipt.backups[index].path), previous);
        previous = hash(&fixture.executable);
        assert_ne!(previous, fixture.original);
    }
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign-stale-image");
    let mut third = fixture.start("1.3.0", 0, "normal");
    assert!(
        !wait_child(&mut third).success(),
        "fixed two-image retention cap must refuse"
    );
    assert_eq!(fixture.receipt().version, "1.2.0");
    assert_eq!(hash(&fixture.executable), previous);
    assert!(!fixture.lock().exists());
    assert!(!fixture.root.join(".fixture-parent-ready.json").exists());
}

#[test]
fn postready_late_error_keeps_outer_guard_alive_until_main_finishes() {
    for mode in ["late-error", "late-schema-error"] {
        let fixture = Fixture::new();
        let mut child = fixture.start("1.1.0", 1600, mode);
        fixture.ready(&mut child);
        std::thread::sleep(Duration::from_millis(600));
        assert!(child.0.try_wait().unwrap().is_none());
        assert_eq!(hash(&fixture.executable), fixture.original);
        assert_eq!(fixture.receipt().version, "1.0.0");
        assert!(!wait_child(&mut child).success());
        fixture.complete("1.1.0");
    }
}

#[test]
fn receipt_failure_after_binary_commit_is_reconciled_without_replacement_replay() {
    let fixture = Fixture::new();
    fixture.fault("receipt-fail");
    let mut child = fixture.start("1.1.0", 300, "normal");
    let transaction = fixture.ready(&mut child);
    assert!(wait_child(&mut child).success());
    fixture.inactive_lock();
    assert_ne!(hash(&fixture.executable), fixture.original);
    assert_eq!(fixture.receipt().version, "1.0.0");
    assert!(transaction.join("applied.json").exists());
    let applied = hash(&fixture.executable);
    fixture.clear_fault();
    let result = reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest)
        .unwrap()
        .unwrap();
    assert_eq!(result.state, HelperState::Reconciled);
    assert!(result.installed && result.receipt_committed && !result.recovery_required);
    assert_eq!(hash(&fixture.executable), applied);
    fixture.complete("1.1.0");
    let mut next = fixture.start("1.2.0", 300, "normal");
    fixture.ready(&mut next);
    assert!(wait_child(&mut next).success());
    fixture.complete("1.2.0");
    assert_eq!(fixture.receipt().backups.len(), 2);
}

#[test]
fn rename_failure_and_helper_death_restore_the_exact_runnable_path_on_reconcile() {
    for fault in ["old-rename-fail", "old-rename-death"] {
        let fixture = Fixture::new();
        fixture.fault(fault);
        let mut child = fixture.start("1.1.0", 300, "normal");
        let transaction = fixture.ready(&mut child);
        assert!(wait_child(&mut child).success());
        fixture.inactive_lock();
        assert_eq!(fixture.receipt().version, "1.0.0");
        if fault == "old-rename-death" {
            assert!(!fixture.executable.exists());
            assert_eq!(hash(&transaction.join("previous.bin")), fixture.original);
        } else {
            assert_eq!(hash(&fixture.executable), fixture.original);
        }
        fixture.clear_fault();
        let result = reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest)
            .unwrap()
            .unwrap();
        assert_eq!(result.state, HelperState::Reconciled);
        assert!(!result.installed && !result.receipt_committed && !result.recovery_required);
        assert_eq!(hash(&fixture.executable), fixture.original);
        assert!(!transaction.exists());
        assert!(!fixture.lock().exists());
        for version in ["1.1.0", "1.2.0"] {
            let mut next = fixture.start(version, 300, "normal");
            fixture.ready(&mut next);
            assert!(wait_child(&mut next).success());
            fixture.complete(version);
        }
    }
}

#[test]
fn unknown_lock_and_package_manager_custody_and_unsigned_requests_never_mutate() {
    for field in ["lock", "custody", "helper_sha256", "schema"] {
        let fixture = Fixture::new();
        if field == "lock" {
            fs::write(fixture.lock(), b"foreign-or-legacy-lock").unwrap();
        } else {
            let path = fixture.root.join(".fab-rs-install.json");
            let mut receipt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            receipt[field] = match field {
                "schema" => json!(1),
                "custody" => json!("package-manager"),
                _ => json!("0".repeat(64)),
            };
            write_private(&path, &serde_json::to_vec(&receipt).unwrap());
        }
        let mut child = fixture.start("1.1.0", 0, "normal");
        assert!(!wait_child(&mut child).success());
        assert_eq!(hash(&fixture.executable), fixture.original);
        if field == "lock" {
            assert_eq!(fs::read(fixture.lock()).unwrap(), b"foreign-or-legacy-lock");
            assert!(matches!(
                reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest),
                Err(Error::Lock)
            ));
        }
    }
    let fixture = Fixture::new();
    let unsigned = fixture.root.join("unsigned.json");
    write_private(
        &unsigned,
        br#"{"key":"caller-key","helper":"untrusted-path"}"#,
    );
    let mut helper = OwnedChild(
        Command::new(&fixture.helper)
            .args(["--reconcile"])
            .arg(unsigned)
            .arg("01".repeat(32))
            .env_clear()
            .current_dir(&fixture.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(!wait_child(&mut helper).success());
    assert_eq!(hash(&fixture.executable), fixture.original);
}

#[test]
fn active_helper_is_pending_and_major_upgrades_require_explicit_permission() {
    let fixture = Fixture::new();
    let mut refused = fixture.start("2.0.0", 0, "normal");
    assert!(!wait_child(&mut refused).success());
    assert!(!fixture.lock().exists());
    let mut allowed = fixture.start("2.0.0", 800, "allow-major");
    fixture.ready(&mut allowed);
    assert!(matches!(
        reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest),
        Err(Error::HelperPending)
    ));
    assert_eq!(hash(&fixture.executable), fixture.original);
    assert!(wait_child(&mut allowed).success());
    fixture.complete("2.0.0");
}

#[test]
fn postready_cancellation_and_asset_tampering_never_apply_or_delete_unknown_bytes() {
    for mode in ["cancel-ready", "tamper-stage"] {
        let fixture = Fixture::new();
        let mut child = fixture.start("1.1.0", 800, mode);
        let transaction = fixture.ready(&mut child);
        let stage = transaction.join("asset.bin");
        let original = fs::read(&stage).unwrap();
        if mode == "tamper-stage" {
            let mut tampered = original.clone();
            *tampered.last_mut().unwrap() ^= 1;
            fs::write(&stage, &tampered).unwrap();
        }
        let status = wait_child(&mut child);
        assert_eq!(status.success(), mode == "tamper-stage");
        fixture.inactive_lock();
        assert_eq!(hash(&fixture.executable), fixture.original);
        assert_eq!(fixture.receipt().version, "1.0.0");
        if mode == "tamper-stage" {
            let tampered = fs::read(&stage).unwrap();
            assert!(matches!(
                reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest),
                Err(Error::RecoveryRequired)
            ));
            assert_eq!(fs::read(&stage).unwrap(), tampered);
            fs::write(&stage, &original).unwrap();
        }
        let result = reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest)
            .unwrap()
            .unwrap();
        assert!(!result.installed);
        assert!(!transaction.exists());
        assert_eq!(hash(&fixture.executable), fixture.original);
    }
}

#[test]
fn independent_helper_rejects_tampered_signature_and_preserves_foreign_journal_files() {
    let fixture = Fixture::new();
    fixture.fault("receipt-fail");
    let mut child = fixture.start("1.1.0", 300, "normal");
    let transaction = fixture.ready(&mut child);
    assert!(wait_child(&mut child).success());
    fixture.inactive_lock();
    fixture.clear_fault();
    let signature = transaction.join("manifest.sig");
    let original = fs::read(&signature).unwrap();
    let mut bad = original.clone();
    bad[0] ^= 1;
    fs::write(&signature, bad).unwrap();
    let applied = hash(&fixture.executable);
    assert!(matches!(
        reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest),
        Err(Error::RecoveryRequired)
    ));
    assert_eq!(fixture.receipt().version, "1.0.0");
    assert_eq!(hash(&fixture.executable), applied);
    assert!(fixture.lock().exists());
    fs::write(signature, original).unwrap();
    let foreign = transaction.join("foreign-unknown-file");
    fs::write(&foreign, b"not-updater-owned").unwrap();
    assert!(matches!(
        reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest),
        Err(Error::RecoveryRequired)
    ));
    assert_eq!(fs::read(&foreign).unwrap(), b"not-updater-owned");
    assert!(fixture.lock().exists());
    // This test created the foreign file. Production never removes it.
    fs::remove_file(foreign).unwrap();
    let result = reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest)
        .unwrap()
        .unwrap();
    assert!(result.installed && result.receipt_committed);
    assert_eq!(hash(&fixture.executable), applied);
    fixture.complete("1.1.0");
}

#[test]
fn parent_lifetime_timeout_never_applies_and_can_reconcile_its_exact_stage() {
    let fixture = Fixture::new();
    let mut child = fixture.start("1.1.0", 32_000, "normal");
    fixture.ready(&mut child);
    wait_until(Duration::from_secs(35), || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.lease())
            .is_ok_and(|file| file.try_lock().is_ok())
    });
    assert!(child.0.try_wait().unwrap().is_none());
    assert_eq!(hash(&fixture.executable), fixture.original);
    assert!(wait_child(&mut child).success());
    let result = reconcile(&fixture.executable, &fixture.helper, fixture.helper_digest)
        .unwrap()
        .unwrap();
    assert!(!result.installed);
    assert_eq!(hash(&fixture.executable), fixture.original);
    assert!(!fixture.lock().exists());
}
