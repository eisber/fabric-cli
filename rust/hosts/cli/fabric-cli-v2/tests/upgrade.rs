// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Apply contracts using only injected executors and synthetic signed metadata.
//! No downloaded bytes, installed helper, network or filesystem are executed.

#![deny(unnameable_test_items)]

use std::cell::Cell;

use fabric_cli_v2::{
    CliError, Control, EffectKind, EffectState, Status,
    upgrade::{
        UpgradeExecutor, UpgradeObservation, UpgradeRequest, UpgradeSession, execute, target_tag,
    },
};

#[derive(Default)]
struct TestControl {
    cancelled: Cell<bool>,
    timeout: Cell<bool>,
}

impl Control for TestControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled.get()
    }

    fn deadline_elapsed(&self) -> bool {
        self.timeout.get()
    }
}

struct CheckedMetadata;
struct DownloadedBytes;

struct Executor<'a> {
    control: &'a TestControl,
    events: Vec<&'static str>,
    tag: Option<String>,
    major: bool,
    fail: Option<&'static str>,
    cancel_after: Option<&'static str>,
    recovered: Option<UpgradeObservation>,
    queued: UpgradeObservation,
}

impl<'a> Executor<'a> {
    fn new(control: &'a TestControl) -> Self {
        Self {
            control,
            events: Vec::new(),
            tag: None,
            major: false,
            fail: None,
            cancel_after: None,
            recovered: None,
            queued: UpgradeObservation::Pending {
                candidate_version: Some("0.2.0".to_owned()),
            },
        }
    }

    fn stage(&mut self, stage: &'static str) -> Result<(), CliError> {
        self.events.push(stage);
        if self.cancel_after == Some(stage) {
            self.control.cancelled.set(true);
        }
        if self.fail == Some(stage) {
            return Err(CliError::new(
                "InjectedFailure",
                "The selected stage failed.",
            ));
        }
        Ok(())
    }
}

impl UpgradeExecutor for Executor<'_> {
    type Checked = CheckedMetadata;
    type Downloaded = DownloadedBytes;

    fn preflight(&mut self) -> Result<Option<UpgradeObservation>, CliError> {
        self.stage("preflight")?;
        Ok(self.recovered.take())
    }

    fn check(&mut self, tag: Option<&str>) -> Result<Self::Checked, CliError> {
        self.stage("check")?;
        self.tag = tag.map(str::to_owned);
        Ok(CheckedMetadata)
    }

    fn requires_major(&self, _: &Self::Checked) -> bool {
        self.major
    }

    fn download(&mut self, _: Self::Checked) -> Result<Self::Downloaded, CliError> {
        self.stage("download")?;
        Ok(DownloadedBytes)
    }

    fn queue(&mut self, _: Self::Downloaded) -> Result<UpgradeObservation, CliError> {
        self.stage("queue")?;
        Ok(self.queued.clone())
    }
}

struct Denied(&'static str);

impl UpgradeExecutor for Denied {
    type Checked = ();
    type Downloaded = ();

    fn preflight(&mut self) -> Result<Option<UpgradeObservation>, CliError> {
        Err(CliError::new(
            self.0,
            "The required trusted installation input is absent.",
        ))
    }

    fn check(&mut self, _: Option<&str>) -> Result<(), CliError> {
        panic!("a failed preflight must not inspect releases")
    }

    fn requires_major(&self, _: &()) -> bool {
        panic!("a failed preflight must not inspect candidates")
    }

    fn download(&mut self, _: ()) -> Result<(), CliError> {
        panic!("a failed preflight must not download")
    }

    fn queue(&mut self, _: ()) -> Result<UpgradeObservation, CliError> {
        panic!("a failed preflight must not queue")
    }
}

#[test]
fn canonical_versions_map_to_exact_rust_tags_without_accepting_other_inputs() {
    assert_eq!(target_tag(&UpgradeRequest::default()).unwrap(), None);
    for version in ["0.2.0", "1.2.3-preview.1", "1.2.3+build.5"] {
        let request = UpgradeRequest {
            target_version: Some(version.to_owned()),
        };
        assert_eq!(
            target_tag(&request).unwrap(),
            Some(format!("rust-v{version}"))
        );
    }
    for version in [
        "",
        "1",
        "1.2",
        "01.2.3",
        "v1.2.3",
        "rust-v1.2.3",
        "^1.2.3",
        "1.2.3 ",
        "https://private-marker.example/update",
        "1.2.3/../program.exe",
    ] {
        let control = TestControl::default();
        let mut executor = Executor::new(&control);
        let error = execute(
            &UpgradeRequest {
                target_version: Some(version.to_owned()),
            },
            &mut executor,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(!error.message.contains("private-marker"));
        assert!(executor.events.is_empty());
        assert!(error.effects.is_empty());
    }
}

#[test]
fn bare_upgrade_is_apply_not_a_successful_metadata_check() {
    let control = TestControl::default();
    let mut executor = Executor::new(&control);
    let result = execute(&UpgradeRequest::default(), &mut executor, &control).unwrap();
    assert_eq!(executor.events, ["preflight", "check", "download", "queue"]);
    assert_eq!(executor.tag, None);
    assert_eq!(result.status, Status::Accepted);
    assert_eq!(result.data["installed"], false);
    assert_eq!(result.data["completion_confirmed"], false);
    assert_eq!(result.data["exit_required"], true);
    assert_eq!(result.data["restart_confirmed"], false);
    assert_eq!(result.data["health_verified"], false);
    assert_eq!(result.effects.len(), 1);
    assert_eq!(result.effects[0].kind, EffectKind::Installation);
    assert_eq!(result.effects[0].state, EffectState::Pending);
}

#[test]
fn target_version_selects_one_exact_tag_without_a_major_override() {
    let control = TestControl::default();
    let mut executor = Executor::new(&control);
    executor.major = true;
    let error = execute(
        &UpgradeRequest {
            target_version: Some("2.0.0".to_owned()),
        },
        &mut executor,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.code, "UpgradeMajorVersion");
    assert_eq!(executor.tag.as_deref(), Some("rust-v2.0.0"));
    assert_eq!(executor.events, ["preflight", "check"]);
    assert!(error.effects.is_empty());
}

#[test]
fn same_major_explicit_version_reaches_apply_once() {
    let control = TestControl::default();
    let mut executor = Executor::new(&control);
    let result = execute(
        &UpgradeRequest {
            target_version: Some("0.2.0".to_owned()),
        },
        &mut executor,
        &control,
    )
    .unwrap();
    assert_eq!(executor.tag.as_deref(), Some("rust-v0.2.0"));
    assert_eq!(executor.events, ["preflight", "check", "download", "queue"]);
    assert_eq!(result.status, Status::Accepted);
}

#[test]
fn checked_downloaded_and_failed_queue_never_become_installed_results() {
    for (stage, expected) in [
        ("preflight", vec!["preflight"]),
        ("check", vec!["preflight", "check"]),
        ("download", vec!["preflight", "check", "download"]),
        ("queue", vec!["preflight", "check", "download", "queue"]),
    ] {
        let control = TestControl::default();
        let mut executor = Executor::new(&control);
        executor.fail = Some(stage);
        let error = execute(&UpgradeRequest::default(), &mut executor, &control).unwrap_err();
        assert_eq!(error.code, "InjectedFailure");
        assert_eq!(executor.events, expected);
        assert!(error.effects.is_empty());
    }
}

#[test]
fn trust_and_custody_failures_stop_before_release_inspection() {
    for code in ["NotConfigured", "UpgradeCustody"] {
        let error = execute(
            &UpgradeRequest::default(),
            &mut Denied(code),
            &TestControl::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, code);
        assert!(error.effects.is_empty());
    }
}

#[test]
fn cancellation_and_timeout_prevent_later_apply_stages() {
    for stage in ["preflight", "check", "download"] {
        let control = TestControl::default();
        let mut executor = Executor::new(&control);
        executor.cancel_after = Some(stage);
        let error = execute(&UpgradeRequest::default(), &mut executor, &control).unwrap_err();
        assert_eq!(error.code, "Cancelled");
        assert_eq!(executor.events.last().copied(), Some(stage));
        assert!(!executor.events.contains(&"queue"));
    }
    for (cancelled, timeout, code) in [(true, false, "Cancelled"), (false, true, "Timeout")] {
        let control = TestControl {
            cancelled: Cell::new(cancelled),
            timeout: Cell::new(timeout),
        };
        let mut executor = Executor::new(&control);
        let error = execute(&UpgradeRequest::default(), &mut executor, &control).unwrap_err();
        assert_eq!(error.code, code);
        assert!(executor.events.is_empty());
    }
}

#[test]
fn cancellation_after_helper_acknowledgement_is_not_a_rollback_claim() {
    let control = TestControl::default();
    let mut executor = Executor::new(&control);
    executor.cancel_after = Some("queue");
    let result = execute(&UpgradeRequest::default(), &mut executor, &control).unwrap();
    assert!(control.is_cancelled());
    assert_eq!(result.status, Status::Accepted);
    assert_eq!(result.effects[0].state, EffectState::Pending);
    assert_eq!(result.data["installed"], false);
}

#[test]
fn reconciliation_reports_installed_receipt_and_recovery_facts_separately() {
    for (installed, receipt_committed, recovery_required, status, effect) in [
        (true, true, false, Status::Succeeded, EffectState::Completed),
        (true, false, true, Status::Partial, EffectState::Unknown),
        (
            false,
            false,
            false,
            Status::Partial,
            EffectState::NotStarted,
        ),
    ] {
        let control = TestControl::default();
        let mut executor = Executor::new(&control);
        executor.recovered = Some(UpgradeObservation::Reconciled {
            candidate_version: "0.2.0".to_owned(),
            installed,
            receipt_committed,
            recovery_required,
        });
        let result = execute(&UpgradeRequest::default(), &mut executor, &control).unwrap();
        assert_eq!(executor.events, ["preflight"]);
        assert_eq!(result.status, status);
        assert_eq!(result.data["installed"], installed);
        assert_eq!(result.data["receipt_committed"], receipt_committed);
        assert_eq!(result.data["recovery_required"], recovery_required);
        assert_eq!(result.data["release_check_performed"], false);
        assert_eq!(result.data["restart_confirmed"], false);
        assert_eq!(result.effects[0].state, effect);
    }
}

#[test]
fn reconciling_a_different_prior_candidate_does_not_satisfy_requested_target() {
    let control = TestControl::default();
    let mut executor = Executor::new(&control);
    executor.recovered = Some(UpgradeObservation::Reconciled {
        candidate_version: "0.2.0".to_owned(),
        installed: true,
        receipt_committed: true,
        recovery_required: false,
    });
    let result = execute(
        &UpgradeRequest {
            target_version: Some("0.3.0".to_owned()),
        },
        &mut executor,
        &control,
    )
    .unwrap();
    assert_eq!(result.status, Status::Partial);
    assert_eq!(result.data["candidate_version"], "0.2.0");
    assert_eq!(result.data["requested_target_version"], "0.3.0");
    assert_eq!(executor.events, ["preflight"]);
}

#[test]
fn active_prior_helper_and_unknown_outcomes_never_claim_completion_or_retry() {
    for observation in [
        UpgradeObservation::Pending {
            candidate_version: None,
        },
        UpgradeObservation::Unknown,
    ] {
        let control = TestControl::default();
        let mut executor = Executor::new(&control);
        executor.recovered = Some(observation);
        let result = execute(&UpgradeRequest::default(), &mut executor, &control).unwrap();
        assert_eq!(executor.events, ["preflight"]);
        assert!(result.data["installed"].is_null());
        assert_eq!(result.data["completion_confirmed"], false);
        assert!(matches!(result.status, Status::Accepted | Status::Unknown));
    }
}

#[test]
fn lifetime_construction_never_queues_anything() {
    let mut lifetime = UpgradeSession::new();
    assert!(!lifetime.is_pending());
    lifetime.cancel().unwrap();
    assert!(!lifetime.is_pending());
}

#[test]
fn session_validates_before_any_native_profile_or_host_effect() {
    let mut session = UpgradeSession::new();
    let error = session
        .run(&UpgradeRequest {
            target_version: Some("https://private-marker.example/update".to_owned()),
        })
        .unwrap_err();
    assert_eq!(error.code, "InvalidInput");
    assert!(!error.message.contains("private-marker"));
    assert!(error.effects.is_empty());
    assert!(!session.is_pending());
}

#[cfg(not(feature = "self-update"))]
#[test]
fn missing_update_host_is_an_error_not_an_inspection_fallback() {
    let mut lifetime = UpgradeSession::new();
    let error = lifetime.run(&UpgradeRequest::default()).unwrap_err();
    assert_eq!(error.code, "UnsupportedCapability");
    assert!(error.effects.is_empty());
    assert!(!lifetime.is_pending());
}

#[cfg(feature = "self-update")]
#[test]
fn signed_v1_assets_cannot_satisfy_the_distinct_v2_variant() {
    use fabric_cli_update::{
        Channel, Error, Identity, Limits, Repository, Target, Trust, VerificationContext,
        verify_manifest,
    };
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::json;

    let signer = Ed25519KeyPair::from_seed_unchecked(&[23; 32]).unwrap();
    let public_key: [u8; 32] = signer.public_key().as_ref().try_into().unwrap();
    let trust = Trust::new(
        Repository::new("microsoft", "fabric-cli").unwrap(),
        Some(public_key),
    )
    .unwrap();
    let variant = if cfg!(feature = "windows-broker") {
        "cli-v2+self-update+windows-broker"
    } else {
        "cli-v2+self-update"
    };
    let identity = Identity::new("0.1.0", Channel::Stable, Target::WindowsX64, variant).unwrap();
    for (candidate_variant, accepted) in [("self-update", false), (variant, true)] {
        let bytes = serde_json::to_vec(&json!({
            "protocol": 1,
            "product": "fab-rs",
            "repository": "microsoft/fabric-cli",
            "tag": "rust-v0.2.0",
            "version": "0.2.0",
            "channel": "stable",
            "target": "x86_64-pc-windows-msvc",
            "variant": candidate_variant,
            "asset": "fab-cli-v2.exe",
            "bytes": 512,
            "sha256": "a1".repeat(32),
            "expires_at": 2000
        }))
        .unwrap();
        let signature = signer.sign(&bytes);
        let result = verify_manifest(
            &bytes,
            signature.as_ref(),
            VerificationContext {
                trust: &trust,
                identity: &identity,
                tag: "rust-v0.2.0",
                prerelease: false,
                now: 1000,
                limits: Limits::default(),
            },
        );
        if accepted {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(Error::Binding)));
        }
    }
}

#[cfg(feature = "self-update")]
mod production_core {
    use std::collections::VecDeque;

    use fabric_cli_update::{
        Channel, DownloadedUpdate, Error, HttpRequest, HttpResponse, HttpTransport, Identity,
        Limits, Repository, ResponseSink, Snapshot, Target, Trust, UpdateEngine,
    };
    use fabric_cli_v2::upgrade::engine::{Installation, apply};
    use ring::{
        digest,
        signature::{Ed25519KeyPair, KeyPair},
    };
    use serde_json::json;

    use super::*;

    impl fabric_cli_update::Control for TestControl {
        fn snapshot(&self) -> Snapshot {
            Snapshot {
                unix_seconds: 1000,
                monotonic_ms: 1,
                cancelled: self.cancelled.get(),
            }
        }
    }

    struct Wire {
        replies: VecDeque<(String, Vec<u8>)>,
        requests: Vec<String>,
    }

    impl HttpTransport for Wire {
        fn get(
            &mut self,
            request: &HttpRequest,
            sink: &mut dyn ResponseSink,
            _: &dyn fabric_cli_update::Control,
        ) -> Result<HttpResponse, Error> {
            self.requests.push(request.url.clone());
            let (expected, body) = self.replies.pop_front().ok_or(Error::Transport)?;
            assert_eq!(request.url, expected);
            for chunk in body.chunks(17) {
                sink.write(chunk)?;
            }
            Ok(HttpResponse {
                status: 200,
                location: None,
            })
        }
    }

    struct Install {
        calls: usize,
        denied: bool,
        expected: Vec<u8>,
        result: UpgradeObservation,
    }

    impl Installation for Install {
        fn preflight(&mut self) -> Result<Option<UpgradeObservation>, CliError> {
            if self.denied {
                Err(CliError::new(
                    "UpgradeCustody",
                    "The installation is managed.",
                ))
            } else {
                Ok(None)
            }
        }

        fn queue(&mut self, update: DownloadedUpdate) -> Result<UpgradeObservation, CliError> {
            self.calls += 1;
            assert_eq!(update.bytes(), self.expected);
            assert!(!update.requires_major_opt_in());
            Ok(self.result.clone())
        }
    }

    struct Case {
        engine: UpdateEngine,
        wire: Wire,
        install: Install,
        request: UpgradeRequest,
    }

    impl Case {
        fn new(version: &str, signed_variant: &str) -> Self {
            let variant = "cli-v2+self-update";
            let signer = Ed25519KeyPair::from_seed_unchecked(&[41; 32]).unwrap();
            let public_key: [u8; 32] = signer.public_key().as_ref().try_into().unwrap();
            let trust = Trust::new(
                Repository::new("microsoft", "fabric-cli").unwrap(),
                Some(public_key),
            )
            .unwrap();
            let identity =
                Identity::new("0.1.0", Channel::Stable, Target::WindowsX64, variant).unwrap();
            let mut binary = vec![0_u8; 256];
            binary[..2].copy_from_slice(b"MZ");
            binary[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
            binary[64..68].copy_from_slice(b"PE\0\0");
            binary[68..70].copy_from_slice(&0x8664_u16.to_le_bytes());
            binary[86..88].copy_from_slice(&0x0002_u16.to_le_bytes());
            binary[88..90].copy_from_slice(&0x020b_u16.to_le_bytes());
            let sha256 = digest::digest(&digest::SHA256, &binary)
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let tag = format!("rust-v{version}");
            let metadata_name = format!("update-x86_64-pc-windows-msvc-{variant}.json");
            let manifest = serde_json::to_vec(&json!({
                "protocol": 1,
                "product": "fab-rs",
                "repository": "microsoft/fabric-cli",
                "tag": tag,
                "version": version,
                "channel": "stable",
                "target": "x86_64-pc-windows-msvc",
                "variant": signed_variant,
                "asset": "fab-cli-v2.exe",
                "bytes": binary.len(),
                "sha256": sha256,
                "expires_at": 2000
            }))
            .unwrap();
            let signature = signer.sign(&manifest).as_ref().to_vec();
            let release = serde_json::to_vec(&json!({
                "id": 123,
                "url": "https://api.github.com/repos/microsoft/fabric-cli/releases/123",
                "html_url": format!("https://github.com/microsoft/fabric-cli/releases/tag/{tag}"),
                "tag_name": tag,
                "draft": false,
                "prerelease": false,
                "assets": [
                    {"name": metadata_name, "size": manifest.len()},
                    {"name": format!("{metadata_name}.sig"), "size": signature.len()},
                    {"name": "fab-cli-v2.exe", "size": binary.len()}
                ]
            }))
            .unwrap();
            let asset_url = |name: &str| {
                format!(
                    "https://github.com/microsoft/fabric-cli/releases/download/{tag}/{}",
                    name.replace('+', "%2B")
                )
            };
            let replies = VecDeque::from([
                (
                    format!(
                        "https://api.github.com/repos/microsoft/fabric-cli/releases/tags/{tag}"
                    ),
                    release,
                ),
                (asset_url(&metadata_name), manifest),
                (asset_url(&format!("{metadata_name}.sig")), signature),
                (asset_url("fab-cli-v2.exe"), binary.clone()),
            ]);
            Self {
                engine: UpdateEngine::new(Some(trust), identity, Limits::default()).unwrap(),
                wire: Wire {
                    replies,
                    requests: Vec::new(),
                },
                install: Install {
                    calls: 0,
                    denied: false,
                    expected: binary,
                    result: UpgradeObservation::Pending {
                        candidate_version: Some(version.to_owned()),
                    },
                },
                request: UpgradeRequest {
                    target_version: Some(version.to_owned()),
                },
            }
        }

        fn run(&mut self) -> Result<fabric_cli_v2::OperationResult, CliError> {
            apply(
                &self.request,
                &self.engine,
                &mut self.wire,
                &mut self.install,
                &TestControl::default(),
            )
        }
    }

    #[test]
    fn real_signed_engine_downloads_then_reports_pending_or_unknown_not_installed() {
        for observation in [
            UpgradeObservation::Pending {
                candidate_version: Some("0.2.0".to_owned()),
            },
            UpgradeObservation::Unknown,
        ] {
            let mut case = Case::new("0.2.0", "cli-v2+self-update");
            case.install.result = observation;
            let result = case.run().unwrap();
            assert_eq!(case.wire.requests.len(), 4);
            assert_eq!(case.install.calls, 1);
            assert!(case.wire.replies.is_empty());
            assert!(matches!(result.status, Status::Accepted | Status::Unknown));
            assert_eq!(result.data["completion_confirmed"], false);
            assert_ne!(result.data["installed"], true);
        }
    }

    #[test]
    fn real_engine_rejects_major_v1_binding_and_tampered_bytes_before_queue() {
        let mut major = Case::new("1.0.0", "cli-v2+self-update");
        assert_eq!(major.run().unwrap_err().code, "UpgradeMajorVersion");
        assert_eq!(major.wire.requests.len(), 3);
        assert_eq!(major.install.calls, 0);

        let mut v1 = Case::new("0.2.0", "self-update");
        assert_eq!(v1.run().unwrap_err().code, "UpgradeBinding");
        assert_eq!(v1.install.calls, 0);

        let mut tampered = Case::new("0.2.0", "cli-v2+self-update");
        tampered.wire.replies.back_mut().unwrap().1[200] ^= 1;
        assert_eq!(tampered.run().unwrap_err().code, "UpgradeIntegrity");
        assert_eq!(tampered.install.calls, 0);
    }

    #[test]
    fn real_engine_adapter_never_contacts_release_host_without_install_custody() {
        let mut case = Case::new("0.2.0", "cli-v2+self-update");
        case.install.denied = true;
        assert_eq!(case.run().unwrap_err().code, "UpgradeCustody");
        assert!(case.wire.requests.is_empty());
        assert_eq!(case.install.calls, 0);
    }
}
