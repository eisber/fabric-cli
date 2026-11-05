// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fabric_cli_update::{
    Channel, Control, CustodyReceipt, DownloadedUpdate, Error, ExitAcknowledgement, FileInstall,
    HelperState, HttpRequest, HttpResponse, HttpTransport, Identity, InstallContext,
    InstallCustody, Limits, ReceiptBinding, Repository, ResponseSink, RunningInstaller, Snapshot,
    Target, Trust, UpdateEngine, reconcile,
};
use ring::rand::SecureRandom;

use super::{
    UpgradeObservation, UpgradeRequest, UpgradeSession,
    engine::{self, Installation},
    transport::AnonymousTransport,
};
use crate::{CliError, EffectKind, EffectState, OperationResult};

const REPOSITORY: &str = "microsoft/fabric-cli";

pub(super) fn run(
    request: &UpgradeRequest,
    lifetime: &mut UpgradeSession,
) -> Result<OperationResult, CliError> {
    let profile = Profile::production(request)?;
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(CliError::new(
            "UpgradeExecutionContext",
            "Run the synchronous upgrade session outside a Tokio execution context; no nested runtime or HTTP request was started.",
        ));
    }
    let control = Arc::new(HostControl::new());
    let engine = UpdateEngine::new(
        Some(profile.trust.clone()),
        profile.identity.clone(),
        Limits::default(),
    )
    .map_err(failure)?;
    let mut installation = NativeInstallation {
        profile: &profile,
        installation: None,
        control: &control,
        lifetime,
    };
    let mut transport = LazyTransport {
        _runtime: None,
        transport: None,
        control: Arc::clone(&control),
    };
    engine::apply(
        request,
        &engine,
        &mut transport,
        &mut installation,
        control.as_ref(),
    )
}

struct RuntimeDriver {
    handle: tokio::runtime::Handle,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    driver: Option<JoinHandle<()>>,
}

impl RuntimeDriver {
    fn new(control: Arc<HostControl>) -> Result<Self, CliError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| failure(Error::Transport))?;
        let handle = runtime.handle().clone();
        let (stop, mut shutdown) = tokio::sync::oneshot::channel();
        let driver = std::thread::Builder::new()
            .name("fabric-cli-v2-update-io".to_owned())
            .spawn(move || {
                runtime.block_on(async {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            control.cancelled.store(true, Ordering::Release);
                            control.cancellation.notify_one();
                            let _ = shutdown.await;
                        }
                        _ = &mut shutdown => {}
                    }
                });
                runtime.shutdown_timeout(Duration::from_secs(1));
            })
            .map_err(|_| failure(Error::Transport))?;
        Ok(Self {
            handle,
            stop: Some(stop),
            driver: Some(driver),
        })
    }
}

impl Drop for RuntimeDriver {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

struct Profile {
    trust: Trust,
    identity: Identity,
    target: Target,
    variant: String,
    receipt: PathBuf,
}

struct BuildPins<'a> {
    key: Option<&'a str>,
    channel: Option<&'a str>,
    variant: Option<&'a str>,
    custody: Option<&'a str>,
    receipt: Option<&'a str>,
}

impl Profile {
    fn production(request: &UpgradeRequest) -> Result<Self, CliError> {
        Self::configured(
            request,
            BuildPins {
                key: option_env!("FAB_CLI_V2_RELEASE_PUBLIC_KEY"),
                channel: option_env!("FAB_CLI_V2_RELEASE_CHANNEL"),
                variant: option_env!("FAB_CLI_V2_RELEASE_VARIANT"),
                custody: option_env!("FAB_CLI_V2_INSTALL_CUSTODY"),
                receipt: option_env!("FAB_CLI_V2_INSTALL_RECEIPT"),
            },
        )
    }

    fn configured(request: &UpgradeRequest, pins: BuildPins<'_>) -> Result<Self, CliError> {
        let missing = || {
            CliError::new(
                "NotConfigured",
                "A trusted CLI v2 release profile is missing: supply build-time FAB_CLI_V2_RELEASE_PUBLIC_KEY, FAB_CLI_V2_RELEASE_CHANNEL, FAB_CLI_V2_RELEASE_VARIANT and standalone installer inputs. No HTTP request was made.",
            )
        };
        let key = decode_hex(pins.key.ok_or_else(missing)?).map_err(|_| missing())?;
        let trust = Trust::new(
            Repository::new("microsoft", "fabric-cli").map_err(failure)?,
            Some(key),
        )
        .map_err(|_| missing())?;
        let channel = match pins.channel {
            Some("stable") => Channel::Stable,
            Some("preview") => Channel::Preview,
            _ => return Err(missing()),
        };
        let variant = compiled_variant();
        if pins.variant != Some(variant.as_str()) {
            return Err(CliError::new(
                "NotConfigured",
                "The approved release profile must pin this exact cli-v2+self-update feature variant. A v1 variant is never eligible; no HTTP request was made.",
            ));
        }
        let target = native_target()?;
        let identity = Identity::new(env!("CARGO_PKG_VERSION"), channel, target, &variant)
            .map_err(|_| missing())?;
        if channel == Channel::Preview && request.target_version.is_none() {
            return Err(CliError::new(
                "UpgradeTargetRequired",
                "The preview channel requires an exact prerelease --target-version.",
            ));
        }
        if let Some(value) = &request.target_version {
            let current =
                semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|_| missing())?;
            let candidate = semver::Version::parse(value).map_err(|_| super::invalid_version())?;
            if current.major != candidate.major {
                return Err(CliError::new(
                    "UpgradeMajorVersion",
                    "CLI v2 upgrade permits the current major version only; no HTTP request was made.",
                ));
            }
        }
        if pins.custody != Some("standalone") {
            return Err(failure(Error::Custody));
        }
        let receipt = pins
            .receipt
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                CliError::new(
                    "UpgradeCustody",
                    "A standalone installer must pin the absolute revision-2 FAB_CLI_V2_INSTALL_RECEIPT at build time. Writability, PATH and user settings do not establish custody.",
                )
            })?;
        Ok(Self {
            trust,
            identity,
            target,
            variant,
            receipt,
        })
    }
}

fn compiled_variant() -> String {
    if cfg!(feature = "windows-broker") {
        "cli-v2+self-update+windows-broker".to_owned()
    } else {
        "cli-v2+self-update".to_owned()
    }
}

fn native_target() -> Result<Target, CliError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(Target::WindowsX64),
        ("windows", "aarch64") => Ok(Target::WindowsArm64),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Ok(Target::LinuxX64),
        ("linux", "aarch64") if cfg!(target_env = "gnu") => Ok(Target::LinuxArm64),
        ("macos", "x86_64") => Ok(Target::MacX64),
        ("macos", "aarch64") => Ok(Target::MacArm64),
        _ => Err(failure(Error::UnsupportedTarget)),
    }
}

fn decode_hex(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Custody);
    }
    let mut result = [0; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |byte: u8| {
            if byte.is_ascii_digit() {
                byte - b'0'
            } else {
                byte.to_ascii_lowercase() - b'a' + 10
            }
        };
        result[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(result)
}

struct NativeInstallation<'a> {
    profile: &'a Profile,
    installation: Option<CustodyReceipt>,
    control: &'a Arc<HostControl>,
    lifetime: &'a mut UpgradeSession,
}

struct LazyTransport {
    _runtime: Option<RuntimeDriver>,
    transport: Option<AnonymousTransport>,
    control: Arc<HostControl>,
}

impl HttpTransport for LazyTransport {
    fn get(
        &mut self,
        request: &HttpRequest,
        sink: &mut dyn ResponseSink,
        control: &dyn Control,
    ) -> Result<HttpResponse, Error> {
        if self.transport.is_none() {
            let runtime =
                RuntimeDriver::new(Arc::clone(&self.control)).map_err(|_| Error::Transport)?;
            self.transport = Some(
                AnonymousTransport::new(runtime.handle.clone(), Arc::clone(&self.control))
                    .map_err(|_| Error::Transport)?,
            );
            self._runtime = Some(runtime);
        }
        self.transport
            .as_mut()
            .ok_or(Error::Transport)?
            .get(request, sink, control)
    }
}

impl NativeInstallation<'_> {
    fn receipt(&self) -> Result<&CustodyReceipt, CliError> {
        self.installation
            .as_ref()
            .ok_or_else(|| failure(Error::Custody))
    }
}

impl Installation for NativeInstallation<'_> {
    fn preflight(&mut self) -> Result<Option<UpgradeObservation>, CliError> {
        if self.lifetime.is_pending() {
            return Ok(Some(UpgradeObservation::Pending {
                candidate_version: None,
            }));
        }
        let executable = std::env::current_exe().map_err(|_| failure(Error::Custody))?;
        let owned = CustodyReceipt::read_owned(
            &self.profile.receipt,
            &executable,
            REPOSITORY,
            self.profile.target,
            &self.profile.variant,
            self.control.as_ref(),
        )
        .map_err(failure)?;
        match reconcile(
            &owned.executable,
            &owned.helper,
            decode_hex(&owned.helper_sha256).map_err(failure)?,
        ) {
            Ok(Some(result)) => {
                if result.state != HelperState::Reconciled {
                    return Ok(Some(UpgradeObservation::Unknown));
                }
                return Ok(Some(UpgradeObservation::Reconciled {
                    candidate_version: result.version,
                    installed: result.installed,
                    receipt_committed: result.receipt_committed,
                    recovery_required: result.recovery_required,
                }));
            }
            Err(Error::HelperPending) => {
                return Ok(Some(UpgradeObservation::Pending {
                    candidate_version: None,
                }));
            }
            Err(error) => return Err(failure(error)),
            Ok(None) => {}
        }
        self.installation = Some(
            CustodyReceipt::load(
                &self.profile.receipt,
                &owned.executable,
                ReceiptBinding {
                    repository: REPOSITORY,
                    version: env!("CARGO_PKG_VERSION"),
                    target: self.profile.target,
                    variant: &self.profile.variant,
                },
                self.control.as_ref(),
            )
            .map_err(failure)?,
        );
        Ok(None)
    }

    fn queue(&mut self, downloaded: DownloadedUpdate) -> Result<UpgradeObservation, CliError> {
        let receipt = self.receipt()?;
        let mut nonce = [0; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| failure(Error::Io))?;
        let installation = FileInstall::new(
            receipt.executable.clone(),
            decode_hex(&receipt.sha256).map_err(failure)?,
            nonce,
        )
        .map_err(failure)?;
        let mut installer = RunningInstaller::new(
            installation,
            ExitAcknowledgement::ExitImmediatelyAndRetainRecovery,
        );
        let pending = installer
            .queue(
                &downloaded,
                InstallContext {
                    custody: InstallCustody::Standalone,
                    budget_ms: 30_000,
                    allow_major: false,
                },
                self.control.as_ref(),
                &mut self.lifetime.parent,
            )
            .map_err(|error| {
                let result = failure(error);
                if self.lifetime.is_pending() && result.effects.is_empty() {
                    result.with_effect(EffectKind::Installation, EffectState::Unknown)
                } else {
                    result
                }
            })?;
        Ok(UpgradeObservation::Pending {
            candidate_version: Some(pending.version),
        })
    }
}

pub(super) fn failure(error: Error) -> CliError {
    let (code, message) = match error {
        Error::TrustRequired => (
            "NotConfigured",
            "The independently approved CLI v2 publisher profile is unavailable; no trust fallback is permitted.",
        ),
        Error::Custody => (
            "UpgradeCustody",
            "Only the actual executable's protected standalone v2 installer receipt and fixed digest-pinned helper authorize replacement; managed or unknown installations must use their installer.",
        ),
        Error::MajorUpgradeRequiresOptIn => (
            "UpgradeMajorVersion",
            "CLI v2 upgrade is same-major only; there is no cross-major override.",
        ),
        Error::Cancelled => (
            "Cancelled",
            "The upgrade was cancelled; cancellation does not prove rollback.",
        ),
        Error::Deadline => ("Timeout", "The upgrade exceeded its finite deadline."),
        Error::ReleaseUnavailable => (
            "NotConfigured",
            "The fixed Microsoft Rust publication family has no matching signed CLI v2 release profile. V1, Python and unsigned releases are never substituted.",
        ),
        Error::Signature => (
            "UpgradeSignature",
            "The manifest signature does not match the independently pinned key.",
        ),
        Error::Expired => (
            "UpgradeExpired",
            "The signed manifest is expired or outside its allowed validity window.",
        ),
        Error::Binding => (
            "UpgradeBinding",
            "The signed release does not match this exact v2 product/channel/target/variant.",
        ),
        Error::Integrity => (
            "UpgradeIntegrity",
            "Downloaded bytes do not match the signed length and digest.",
        ),
        Error::BinaryFormat => (
            "UpgradeBinary",
            "The asset is not a matching raw native executable.",
        ),
        Error::Ineligible => (
            "UpgradeIneligible",
            "The selected release is not a strictly newer eligible update.",
        ),
        Error::Origin => (
            "UpgradeOrigin",
            "The release origin or redirect is not allowed.",
        ),
        Error::Schema | Error::InvalidIdentity => (
            "UpgradeSchema",
            "The release metadata or requested identity is invalid.",
        ),
        Error::Limit | Error::InvalidLimits => (
            "UpgradeLimit",
            "The update exceeds a configured hard limit.",
        ),
        Error::UnsupportedTarget => (
            "UpgradeUnsupportedTarget",
            "This native target has no approved update mapping.",
        ),
        Error::NativeBoundary | Error::RunningExecutable => (
            "UpgradeNativeBoundary",
            "The exact running-image or installed-helper boundary is unsupported; no alternate program is launched.",
        ),
        Error::Lock | Error::InstallConflict => (
            "UpgradeConflict",
            "The installation lock, bytes or retained recovery paths conflict; foreign state is never reclaimed.",
        ),
        Error::Retention => (
            "UpgradeRetention",
            "The owning installer must reconcile the two retained verified backups before another upgrade.",
        ),
        Error::Io => (
            "UpgradeFilesystem",
            "A bounded upgrade filesystem operation failed.",
        ),
        Error::Install => (
            "UpgradeInstallation",
            "The installation adapter did not establish completion.",
        ),
        Error::Transport | Error::Response => (
            "UpgradeTransport",
            "The anonymous release request failed; no retry or credential fallback was used.",
        ),
        Error::HelperPending => (
            "UpgradePending",
            "The exact helper transaction remains pending.",
        ),
        Error::RecoveryRequired => {
            return CliError::new(
                "UpgradeRecoveryRequired",
                "Native replacement may have moved files. Exit without replay; retain exact-owned recovery evidence for reconciliation.",
            )
            .with_effect(EffectKind::Installation, EffectState::Unknown);
        }
    };
    CliError::new(code, message)
}

pub(super) struct HostControl {
    started: Instant,
    pub(super) cancelled: AtomicBool,
    pub(super) cancellation: tokio::sync::Notify,
}

impl HostControl {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            cancelled: AtomicBool::new(false),
            cancellation: tokio::sync::Notify::new(),
        }
    }
}

impl Control for HostControl {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            monotonic_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            cancelled: self.cancelled.load(Ordering::Acquire),
        }
    }
}

impl crate::Control for HostControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn deadline_elapsed(&self) -> bool {
        self.started.elapsed().as_secs() >= 90
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins<'a>(key: &'a str, variant: &'a str) -> BuildPins<'a> {
        BuildPins {
            key: Some(key),
            channel: Some("stable"),
            variant: Some(variant),
            custody: Some("standalone"),
            receipt: Some(if cfg!(windows) {
                r"C:\Owned\.fab-rs-install.json"
            } else {
                "/owned/.fab-rs-install.json"
            }),
        }
    }

    #[test]
    fn missing_key_channel_or_distinct_variant_is_a_pure_pre_http_error() {
        let key = "a1".repeat(32);
        let variant = compiled_variant();
        for field in ["key", "channel", "variant"] {
            let mut input = pins(&key, &variant);
            match field {
                "key" => input.key = None,
                "channel" => input.channel = None,
                _ => input.variant = Some("self-update"),
            }
            let error = Profile::configured(&UpgradeRequest::default(), input)
                .err()
                .expect("unapproved profile must fail");
            assert_eq!(error.code, "NotConfigured");
        }
        for key in ["0".repeat(64), "g".repeat(64), "a1".repeat(31)] {
            let error = Profile::configured(&UpgradeRequest::default(), pins(&key, &variant))
                .err()
                .expect("invalid test key must fail");
            assert_eq!(error.code, "NotConfigured");
        }
    }

    #[test]
    fn managed_missing_or_relative_custody_is_never_promoted_to_standalone() {
        let key = "a1".repeat(32);
        let variant = compiled_variant();
        for custody in [None, Some("package-manager"), Some("enterprise")] {
            let mut input = pins(&key, &variant);
            input.custody = custody;
            let error = Profile::configured(&UpgradeRequest::default(), input)
                .err()
                .expect("unowned custody must fail");
            assert_eq!(error.code, "UpgradeCustody");
        }
        for receipt in [None, Some("relative-install.json")] {
            let mut input = pins(&key, &variant);
            input.receipt = receipt;
            let error = Profile::configured(&UpgradeRequest::default(), input)
                .err()
                .expect("missing absolute installer record must fail");
            assert_eq!(error.code, "UpgradeCustody");
        }
    }

    #[test]
    fn unknown_replacement_errors_keep_effect_evidence_and_never_report_unchanged() {
        let error = failure(Error::RecoveryRequired);
        assert_eq!(error.code, "UpgradeRecoveryRequired");
        assert_eq!(error.effects.len(), 1);
        assert_eq!(error.effects[0].state, EffectState::Unknown);
    }
}
