// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in anonymous update composition; no SDK credentials or global runtime.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use azure_core::http::{HttpClient, Method, Request, Url};
use fabric_cli::parser::{Invocation, UpdateAction, UpdateCommand};
use fabric_cli_update::{
    AutomaticContext, Channel, CheckSchedule, Control, CustodyReceipt, Error, ExitAcknowledgement,
    FileInstall, HttpRequest, HttpResponse, HttpTransport, Identity, InstallContext,
    InstallCustody, Limits, ParentLifetime, Policy, ReceiptBinding, ReleaseSelector, Repository,
    ResponseSink, RunningInstaller, Snapshot, StartupDecision, Target, Trust, UpdateEngine,
    is_allowed_release_cdn, reconcile,
};
use fabric_sdk_transport::{NativeHttpClient, ProxyPolicy, TransportOptions, TrustPolicy};
use futures_util::StreamExt;
#[cfg(test)]
use ring::digest;
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{runtime::Handle, sync::oneshot};

use crate::batch_state::CommandOutcome;

const OWNER: &str = "microsoft";
const REPOSITORY: &str = "fabric-cli";
const CHECK_INTERVAL: u64 = 24 * 60 * 60;
const AUTOMATIC_RERUN_EXIT: u8 = 75;

type Failure = (&'static str, &'static str);

/// Run only an explicit update invocation, independently of authentication.
pub(crate) fn run(
    command: &UpdateCommand,
    lifetime: &mut ParentLifetime,
) -> io::Result<CommandOutcome> {
    match perform(command, command.action == UpdateAction::Apply, lifetime) {
        Ok(report) => {
            serde_json::to_writer(io::stdout().lock(), &json!({"update": report}))
                .map_err(io::Error::other)?;
            writeln!(io::stdout().lock())?;
            Ok(explicit_outcome(command.action, 0, lifetime.is_pending()))
        }
        Err((code, message)) => {
            write_failure(code, message)?;
            Ok(explicit_outcome(command.action, 1, lifetime.is_pending()))
        }
    }
}

/// Coordinate an eligible single-command startup before any auth/dispatch.
///
/// Parent integration must call this only from the outer parsed command path,
/// never from recursive batch or REPL dispatch. This function also rejects
/// explicit batch/help/version/update and automation/nonterminal contexts.
pub(crate) fn startup(
    invocation: &Invocation,
    lifetime: &mut ParentLifetime,
) -> io::Result<Option<CommandOutcome>> {
    if lifetime.is_pending() {
        return Ok(Some(CommandOutcome::Stop(AUTOMATIC_RERUN_EXIT)));
    }
    if !startup_invocation_allowed(invocation) {
        return Ok(None);
    }
    let context = AutomaticContext {
        terminal: io::stdin().is_terminal() && io::stdout().is_terminal(),
        help_or_version: false,
        automation: ["CI", "GITHUB_ACTIONS", "TF_BUILD", "FAB_RUST_BATCH"]
            .iter()
            .any(|name| std::env::var_os(name).is_some()),
    };
    if !context.terminal || context.automation {
        return Ok(None);
    }
    let policy_input = match std::env::var("FAB_RUST_UPDATE_POLICY") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            warn_startup(
                "update_configuration",
                "FAB_RUST_UPDATE_POLICY must be valid text.",
            );
            return Ok(None);
        }
    };
    let policy = match selected_policy(policy_input.as_deref()) {
        Ok(policy) => policy,
        Err((code, message)) => {
            warn_startup(code, message);
            return Ok(None);
        }
    };
    let mut schedule = CheckSchedule::default();
    let decision = schedule.startup(policy, context, 0);
    if decision == StartupDecision::Skip {
        return Ok(None);
    }
    let command = automatic_command(decision);
    // Missing publisher trust must fail before touching cadence or HTTP state.
    if let Err((code, message)) = settings(&command) {
        warn_startup(code, message);
        return Ok(None);
    }
    let due = match cadence_attempt() {
        Ok(due) => due,
        Err((code, message)) => {
            warn_startup(code, message);
            return Ok(None);
        }
    };
    if !due {
        return Ok(None);
    }
    match perform(&command, false, lifetime) {
        Ok(report) => {
            let notice_action = if lifetime.is_pending() {
                UpdateAction::Apply
            } else {
                command.action
            };
            let notice = startup_success_notice(&report, notice_action);
            let _ = serde_json::to_writer(io::stderr().lock(), &notice);
            let _ = writeln!(io::stderr().lock());
            Ok(startup_success_outcome(
                command.action,
                lifetime.is_pending(),
            ))
        }
        Err((code, message)) => {
            warn_startup(code, message);
            // A native call can move the executable before failing. Stop now;
            // never continue ordinary dispatch on a recovery-required state.
            Ok(startup_failure_outcome(code, lifetime.is_pending()))
        }
    }
}

fn automatic_command(decision: StartupDecision) -> UpdateCommand {
    UpdateCommand {
        action: if decision == StartupDecision::CheckAndInstall {
            UpdateAction::Apply
        } else {
            UpdateAction::Check
        },
        tag: None,
        allow_major: false,
    }
}

fn require_major_permission(command: &UpdateCommand, requires_major: bool) -> Result<(), Error> {
    if command.allow_major && command.action != UpdateAction::Apply {
        return Err(Error::InvalidIdentity);
    }
    if command.action == UpdateAction::Apply && requires_major && !command.allow_major {
        return Err(Error::MajorUpgradeRequiresOptIn);
    }
    Ok(())
}

fn startup_invocation_allowed(invocation: &Invocation) -> bool {
    match invocation {
        Invocation::Display(_) | Invocation::Batch(_) | Invocation::Update(_) => false,
        Invocation::Local(local)
            if matches!(&local.command, fabric_cli_local::LocalCommand::Version) =>
        {
            false
        }
        #[cfg(feature = "windows-broker")]
        Invocation::Auth(_) => false,
        _ => true,
    }
}

fn explicit_outcome(action: UpdateAction, status: u8, helper_pending: bool) -> CommandOutcome {
    if action == UpdateAction::Apply || helper_pending {
        CommandOutcome::Stop(status)
    } else {
        CommandOutcome::Returned(status)
    }
}

fn startup_failure_outcome(code: &str, helper_pending: bool) -> Option<CommandOutcome> {
    (code == "update_recovery_required" || helper_pending).then_some(CommandOutcome::Stop(1))
}

fn startup_success_outcome(action: UpdateAction, helper_pending: bool) -> Option<CommandOutcome> {
    (action == UpdateAction::Apply || helper_pending)
        .then_some(CommandOutcome::Stop(AUTOMATIC_RERUN_EXIT))
}

fn startup_success_notice(report: &Value, action: UpdateAction) -> Value {
    if action == UpdateAction::Apply {
        json!({
            "update_notice":report,
            "code":"update_exit_rerun_required",
            "requested_command_executed":false,
            "rerun_required":true,
            "exit_status":AUTOMATIC_RERUN_EXIT,
            "message":"The update requires this process to exit. Completion is not implied by queuing. The requested command was not executed; rerun it explicitly."
        })
    } else {
        json!({"update_notice":report})
    }
}

fn warn_startup(code: &str, message: &str) {
    // A closed diagnostic stream must not replace the ordinary command's exit.
    // No update success is implied: this only attempts a nonfatal warning.
    let _ = write_failure(code, message);
}

fn write_failure(code: &str, message: &str) -> io::Result<()> {
    serde_json::to_writer(
        io::stderr().lock(),
        &json!({"error":{"code":code,"message":message}}),
    )
    .map_err(io::Error::other)?;
    writeln!(io::stderr().lock())
}

struct Settings {
    engine: UpdateEngine,
    selector: ReleaseSelector,
    installation: Option<BoundInstall>,
}

fn settings(command: &UpdateCommand) -> Result<Settings, Failure> {
    require_major_permission(command, false).map_err(classify)?;
    let key = parse_key(option_env!("FAB_RELEASE_PUBLIC_KEY"))?;
    let trust = Trust::new(
        Repository::new(OWNER, REPOSITORY).map_err(classify)?,
        Some(key),
    )
    .map_err(classify)?;
    let channel = match option_env!("FAB_RELEASE_CHANNEL").unwrap_or("stable") {
        "stable" => Channel::Stable,
        "preview" => Channel::Preview,
        _ => {
            return Err((
                "NotConfigured",
                "The release build has an invalid update channel.",
            ));
        }
    };
    let variant = feature_variant();
    if option_env!("FAB_RELEASE_VARIANT").is_some_and(|pinned| pinned != variant) {
        return Err((
            "NotConfigured",
            "The release build's feature variant does not match this executable.",
        ));
    }
    if command.action == UpdateAction::Apply
        && option_env!("FAB_RELEASE_INSTALL_CUSTODY") != Some("standalone")
    {
        return Err((
            "update_custody",
            "Apply requires a release build marked standalone by its installer. Package-manager or unknown installs must use their owning installer.",
        ));
    }
    let target = native_target()?;
    let installation = if command.action == UpdateAction::Apply {
        let receipt = std::env::var_os("FAB_RUST_UPDATE_INSTALL_RECEIPT")
            .map(PathBuf::from)
            .ok_or((
                "update_custody",
                "Apply requires an explicit FAB_RUST_UPDATE_INSTALL_RECEIPT from the standalone installer; writability or a build marker alone is not custody.",
            ))?;
        let executable = std::env::current_exe().map_err(|_| {
            (
                "update_custody",
                "The actual running executable could not be bound to an installation receipt.",
            )
        })?;
        let owned = CustodyReceipt::read_owned(
            &receipt,
            &executable,
            "microsoft/fabric-cli",
            target,
            &variant,
            &HostControl::new(),
        )
        .map_err(classify)?;
        Some(BoundInstall {
            executable: owned.executable.clone(),
            current_sha256: decode_hex(&owned.sha256).map_err(classify)?,
            receipt_path: receipt,
            helper: owned.helper,
            helper_sha256: decode_hex(&owned.helper_sha256).map_err(classify)?,
        })
    } else {
        None
    };
    let identity =
        Identity::new(env!("CARGO_PKG_VERSION"), channel, target, &variant).map_err(classify)?;
    let selector = match &command.tag {
        Some(tag) => ReleaseSelector::exact_tag(tag).map_err(classify)?,
        None if channel == Channel::Stable => ReleaseSelector::LatestRustStable,
        None => {
            return Err((
                "update_tag_required",
                "Preview checks require an exact rust-vSEMVER tag.",
            ));
        }
    };
    Ok(Settings {
        engine: UpdateEngine::new(Some(trust), identity, Limits::default()).map_err(classify)?,
        selector,
        installation,
    })
}

struct BoundInstall {
    executable: PathBuf,
    current_sha256: [u8; 32],
    receipt_path: PathBuf,
    helper: PathBuf,
    helper_sha256: [u8; 32],
}

fn load_install_receipt(
    receipt: &Path,
    executable: &Path,
    variant: &str,
    target: Target,
    control: &dyn Control,
) -> Result<BoundInstall, Failure> {
    let invalid = || {
        (
            "update_custody",
            "The standalone installation receipt is missing, invalid, package-manager-owned, or does not bind this exact executable/version/target/variant/digest.",
        )
    };
    let owned = CustodyReceipt::load(
        receipt,
        executable,
        ReceiptBinding {
            repository: "microsoft/fabric-cli",
            version: env!("CARGO_PKG_VERSION"),
            target,
            variant,
        },
        control,
    )
    .map_err(|_| invalid())?;
    Ok(BoundInstall {
        executable: owned.executable,
        current_sha256: decode_hex(&owned.sha256).map_err(|_| invalid())?,
        receipt_path: receipt.to_path_buf(),
        helper: owned.helper,
        helper_sha256: decode_hex(&owned.helper_sha256).map_err(|_| invalid())?,
    })
}

fn decode_hex(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Custody);
    }
    let mut result = [0; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        result[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(result)
}

fn parse_key(value: Option<&str>) -> Result<[u8; 32], Failure> {
    let value = value.ok_or((
        "NotConfigured",
        "This build has no pinned release public key. A trusted release build must supply FAB_RELEASE_PUBLIC_KEY at build time; no network request was made.",
    ))?;
    if value.len() != 64 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err((
            "NotConfigured",
            "The build-time release public key must be exactly 32 hexadecimal bytes.",
        ));
    }
    let mut result = [0; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |c: u8| {
            if c.is_ascii_digit() {
                c - b'0'
            } else {
                c.to_ascii_lowercase() - b'a' + 10
            }
        };
        result[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    if result == [0; 32] {
        return Err((
            "NotConfigured",
            "The build-time release public key is invalid.",
        ));
    }
    Ok(result)
}

fn feature_variant() -> String {
    if cfg!(any(
        feature = "interactive",
        feature = "certificate-store-pkcs11",
        feature = "certificate-store-macos",
    )) {
        let mut features = vec!["self-update"];
        for (enabled, name) in [
            (cfg!(feature = "interactive"), "interactive"),
            (
                cfg!(feature = "certificate-store-pkcs11"),
                "certificate-store-pkcs11",
            ),
            (
                cfg!(feature = "certificate-store-macos"),
                "certificate-store-macos",
            ),
            (
                cfg!(feature = "certificate-store-windows"),
                "certificate-store-windows",
            ),
            (cfg!(feature = "headless"), "headless"),
            (cfg!(feature = "native-host"), "native-host"),
            (cfg!(feature = "native-read"), "native-read"),
            (cfg!(feature = "repl"), "repl"),
            (cfg!(feature = "windows-broker"), "windows-broker"),
        ] {
            if enabled {
                features.push(name);
            }
        }
        features.sort_unstable();
        let payload = format!("fabric-cli-feature-v2\0{}", features.join("\0"));
        let digest = ring::digest::digest(&ring::digest::SHA256, payload.as_bytes());
        let hex: String = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        return format!("f2-{}", &hex[..60]);
    }
    let mut features = vec!["self-update"];
    let certificate = cfg!(feature = "certificate-store-windows");
    for (enabled, name) in [
        (certificate, "certificate-store-windows"),
        (cfg!(feature = "headless") && !certificate, "headless"),
        (cfg!(feature = "native-host") && !certificate, "native-host"),
        (cfg!(feature = "native-read"), "native-read"),
        (cfg!(feature = "repl"), "repl"),
        (
            cfg!(feature = "windows-broker") && !(certificate && cfg!(feature = "native-read")),
            "windows-broker",
        ),
    ] {
        if enabled {
            features.push(name);
        }
    }
    features.join("+")
}

fn native_target() -> Result<Target, Failure> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Target::WindowsX64,
        ("windows", "aarch64") => Target::WindowsArm64,
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Target::LinuxX64,
        ("linux", "aarch64") if cfg!(target_env = "gnu") => Target::LinuxArm64,
        ("macos", "x86_64") => Target::MacX64,
        ("macos", "aarch64") => Target::MacArm64,
        _ => {
            return Err((
                "update_unsupported_target",
                "This native target has no approved update asset mapping.",
            ));
        }
    };
    Ok(target)
}

fn perform(
    command: &UpdateCommand,
    capture_interrupt: bool,
    lifetime: &mut ParentLifetime,
) -> Result<Value, Failure> {
    let mut settings = settings(command)?;
    let control = Arc::new(HostControl::new());
    if let Some(bound) = settings.installation.as_ref() {
        match reconcile(&bound.executable, &bound.helper, bound.helper_sha256) {
            Ok(Some(result)) => {
                return serde_json::to_value(result).map_err(|_| classify(Error::RecoveryRequired));
            }
            Err(Error::HelperPending) => {
                return Ok(json!({
                    "state":"pending", "installed":null, "completion_confirmed":false, "exit_required":true,
                    "message":"A verified bounded helper is still processing the existing transaction; completion has not been observed."
                }));
            }
            Err(error) => return Err(classify(error)),
            Ok(None) => {}
        }
        settings.installation = Some(load_install_receipt(
            &bound.receipt_path,
            &bound.executable,
            &feature_variant(),
            native_target()?,
            control.as_ref(),
        )?);
    }
    let http = NativeHttpClient::new(TransportOptions {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(10),
        write_timeout: Duration::from_secs(10),
        pool_idle_timeout: Duration::from_secs(10),
        max_idle_per_host: NonZeroUsize::new(1)
            .ok_or(("update_configuration", "Invalid pool limit."))?,
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    })
    .map_err(|_| {
        (
            "update_transport",
            "The anonymous update TLS transport could not be configured.",
        )
    })?;
    let runtime = UpdateRuntime::new(control.clone(), capture_interrupt)?;
    let mut transport = AnonymousTransport {
        handle: runtime.handle.clone(),
        http: Arc::new(http),
    };
    let checked = settings
        .engine
        .check(&settings.selector, &mut transport, control.as_ref())
        .map_err(classify)?;
    require_major_permission(command, checked.requires_major_opt_in()).map_err(classify)?;
    if command.action == UpdateAction::Check {
        return Ok(json!({
            "state":"available",
            "installed":false,
            "version":checked.manifest().manifest().version,
            "tag":checked.manifest().manifest().tag,
            "repository":"microsoft/fabric-cli",
            "variant":checked.manifest().manifest().variant,
            "requires_major_opt_in":checked.requires_major_opt_in()
        }));
    }
    let downloaded = settings
        .engine
        .download(checked, &mut transport, control.as_ref())
        .map_err(classify)?;
    let bound = settings.installation.ok_or((
        "update_custody",
        "The installation has no validated standalone custody receipt.",
    ))?;
    let mut nonce = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| {
            (
                "update_filesystem",
                "A fresh update lock nonce could not be generated.",
            )
        })?;
    let installation =
        FileInstall::new(bound.executable, bound.current_sha256, nonce).map_err(classify)?;
    let mut installer = RunningInstaller::new(
        installation,
        ExitAcknowledgement::ExitImmediatelyAndRetainRecovery,
    );
    let receipt = installer
        .queue(
            &downloaded,
            InstallContext {
                custody: InstallCustody::Standalone,
                budget_ms: 30_000,
                allow_major: command.allow_major,
            },
            control.as_ref(),
            lifetime,
        )
        .map_err(classify)?;
    Ok(json!({
        "state":"pending_parent_exit",
        "installed":false,
        "exit_required":true,
        "version":receipt.version,
        "transaction":receipt.transaction,
        "helper_cleanup":"pending_parent_exit",
        "health_verified":false
    }))
}

#[cfg(test)]
fn hash_current(path: &Path, control: &dyn Control) -> Result<[u8; 32], Error> {
    let start = control.snapshot().monotonic_ms;
    let mut file = File::open(path).map_err(|_| Error::Io)?;
    let mut digest = digest::Context::new(&digest::SHA256);
    let mut total = 0_usize;
    let mut buffer = [0; 64 * 1024];
    loop {
        let now = control.snapshot();
        if now.cancelled {
            return Err(Error::Cancelled);
        }
        if now.monotonic_ms < start || now.monotonic_ms - start >= 30_000 {
            return Err(Error::Deadline);
        }
        let read = file.read(&mut buffer).map_err(|_| Error::Io)?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read).ok_or(Error::Limit)?;
        if total > 256 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        digest.update(&buffer[..read]);
    }
    let mut result = [0; 32];
    result.copy_from_slice(digest.finish().as_ref());
    Ok(result)
}

fn classify(error: Error) -> Failure {
    match error {
        Error::TrustRequired => (
            "NotConfigured",
            "A pinned publisher configuration is required.",
        ),
        Error::Cancelled => (
            "update_cancelled",
            "The update was cancelled before native replacement.",
        ),
        Error::Deadline => ("update_timeout", "The update exceeded its finite deadline."),
        Error::RecoveryRequired => (
            "update_recovery_required",
            "Native replacement may have moved files. Stop using this process; retain its update lock, verified stage, and rollback for reconciliation.",
        ),
        Error::NativeBoundary => (
            "update_native_boundary",
            "The installed bounded helper or local lifetime-protocol boundary is unsupported.",
        ),
        Error::Custody => (
            "update_custody",
            "Only an explicitly owned standalone executable may be replaced.",
        ),
        Error::Lock => (
            "update_locked",
            "An update or native-helper recovery lock exists or was lost; do not reclaim it automatically.",
        ),
        Error::Ineligible => (
            "update_ineligible",
            "No strictly newer eligible Rust release was selected.",
        ),
        Error::MajorUpgradeRequiresOptIn => (
            "update_major_opt_in_required",
            "Major upgrades require explicit update --apply --allow-major. Automatic installation never opts in.",
        ),
        Error::ReleaseUnavailable => (
            "NotConfigured",
            "The pinned publisher has no matching Rust release or required signed update metadata. Python releases and unsigned assets are never an update fallback.",
        ),
        Error::Signature => (
            "update_signature",
            "The release manifest signature does not match the pinned public key.",
        ),
        Error::Expired => (
            "update_expired",
            "The signed manifest is expired or outside its allowed validity window.",
        ),
        Error::Origin => (
            "update_origin",
            "The release origin or redirect is not allowed.",
        ),
        Error::Integrity => (
            "update_integrity",
            "The downloaded asset does not match its signed size and digest.",
        ),
        Error::BinaryFormat => (
            "update_binary",
            "The asset is not a matching raw native executable.",
        ),
        Error::Binding => (
            "update_binding",
            "The release does not match the exact pinned product/channel/target/variant.",
        ),
        Error::Schema | Error::InvalidIdentity => (
            "update_schema",
            "The release metadata or exact Rust tag is invalid.",
        ),
        Error::Limit | Error::InvalidLimits => (
            "update_limit",
            "The update exceeds a configured hard limit.",
        ),
        Error::UnsupportedTarget => (
            "update_unsupported_target",
            "This native update target is unsupported.",
        ),
        Error::Io => (
            "update_filesystem",
            "The update filesystem operation failed.",
        ),
        Error::InstallConflict => (
            "update_conflict",
            "The installed bytes or retained recovery paths conflict.",
        ),
        Error::RunningExecutable => (
            "update_native_context",
            "Running replacement requires the exact current image and its trusted installed helper; no process-global context is changed.",
        ),
        Error::Retention => (
            "update_retention_full",
            "Two verified prior images are retained. The owning standalone installer must safely reconcile backups and its receipt before another upgrade; unknown or pending recovery files must not be deleted.",
        ),
        Error::HelperPending => (
            "update_pending",
            "A bounded helper still owns this exact transaction; installation completion is not yet confirmed.",
        ),
        Error::Install => (
            "update_install",
            "The selected replacement adapter cannot install this update.",
        ),
        Error::Transport | Error::Response => (
            "update_transport",
            "The anonymous release request failed; no cache or retry fallback was used.",
        ),
    }
}

struct HostControl {
    started: Instant,
    cancelled: AtomicBool,
}

impl HostControl {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            cancelled: AtomicBool::new(false),
        }
    }
}

impl Control for HostControl {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            monotonic_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            cancelled: self.cancelled.load(Ordering::Acquire),
        }
    }
}

struct UpdateRuntime {
    handle: Handle,
    stop: Option<oneshot::Sender<()>>,
    driver: Option<JoinHandle<()>>,
}

impl UpdateRuntime {
    fn new(control: Arc<HostControl>, listen_for_interrupt: bool) -> Result<Self, Failure> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                (
                    "update_runtime",
                    "The dedicated update runtime could not be created.",
                )
            })?;
        let handle = runtime.handle().clone();
        let (stop, mut shutdown) = oneshot::channel();
        let driver = std::thread::Builder::new()
            .name("fabric-update-io".to_owned())
            .spawn(move || {
                runtime.block_on(async {
                    if !listen_for_interrupt {
                        let _ = shutdown.await;
                        return;
                    }
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            control.cancelled.store(true, Ordering::Release);
                            let _ = shutdown.await;
                        }
                        _ = &mut shutdown => {}
                    }
                });
                runtime.shutdown_timeout(Duration::from_secs(1));
            })
            .map_err(|_| {
                (
                    "update_runtime",
                    "The dedicated update I/O driver could not be created.",
                )
            })?;
        Ok(Self {
            handle,
            stop: Some(stop),
            driver: Some(driver),
        })
    }
}

impl Drop for UpdateRuntime {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

struct AnonymousTransport {
    handle: Handle,
    http: Arc<dyn HttpClient>,
}

impl HttpTransport for AnonymousTransport {
    fn get(
        &mut self,
        pending: &HttpRequest,
        sink: &mut dyn ResponseSink,
        control: &dyn Control,
    ) -> Result<HttpResponse, Error> {
        let url = checked_url(&pending.url)?;
        let now = control.snapshot();
        if now.cancelled {
            return Err(Error::Cancelled);
        }
        let remaining = pending
            .deadline_ms
            .checked_sub(now.monotonic_ms)
            .filter(|ms| *ms != 0)
            .ok_or(Error::Deadline)?;
        let mut request = Request::new(url, Method::Get);
        request.insert_header("accept", pending.accept);
        request.insert_header(
            "user-agent",
            concat!("fabric-cli-update/", env!("CARGO_PKG_VERSION")),
        );
        request.insert_header("x-github-api-version", pending.api_version);
        request.insert_header("accept-encoding", "identity");
        self.handle.block_on(async {
            let operation = async {
                let response = self
                    .http
                    .execute_request(&request)
                    .await
                    .map_err(|_| Error::Transport)?;
                let status = u16::from(response.status());
                if response
                    .headers()
                    .get_str(&"content-encoding".into())
                    .is_ok_and(|value| !value.eq_ignore_ascii_case("identity"))
                {
                    return Err(Error::Response);
                }
                if let Ok(value) = response.headers().get_str(&"content-length".into()) {
                    let length = value.parse::<u64>().map_err(|_| Error::Response)?;
                    if length > pending.max_bytes as u64 {
                        return Err(Error::Limit);
                    }
                }
                let location = response
                    .headers()
                    .get_str(&"location".into())
                    .ok()
                    .map(str::to_owned);
                if location.as_ref().is_some_and(|value| value.len() > 4096) {
                    return Err(Error::Origin);
                }
                if status == 200 {
                    let mut body = response.into_body();
                    let mut total = 0_usize;
                    while let Some(chunk) = body.next().await {
                        let chunk = chunk.map_err(|_| Error::Transport)?;
                        total = total.checked_add(chunk.len()).ok_or(Error::Limit)?;
                        if total > pending.max_bytes {
                            return Err(Error::Limit);
                        }
                        sink.write(&chunk)?;
                    }
                }
                Ok(HttpResponse { status, location })
            };
            let cancelled = async {
                loop {
                    if control.snapshot().cancelled {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::select! {
                result = operation => result,
                _ = tokio::time::sleep(Duration::from_millis(remaining)) => Err(Error::Deadline),
                _ = cancelled => Err(Error::Cancelled),
            }
        })
    }
}

fn checked_url(value: &str) -> Result<Url, Error> {
    if value.len() > 4096
        || !value.is_ascii()
        || value
            .bytes()
            .any(|c| c.is_ascii_whitespace() || c.is_ascii_control() || c == b'\\')
    {
        return Err(Error::Origin);
    }
    let url = Url::parse(value).map_err(|_| Error::Origin)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
    {
        return Err(Error::Origin);
    }
    let initial = match url.host_str() {
        Some("api.github.com") => {
            value.starts_with("https://api.github.com/")
                && (url.path() == "/repos/microsoft/fabric-cli/releases"
                    || url.path() == "/repos/microsoft/fabric-cli/releases/latest"
                    || url
                        .path()
                        .starts_with("/repos/microsoft/fabric-cli/releases/tags/rust-v"))
        }
        Some("github.com") => {
            value.starts_with("https://github.com/")
                && url
                    .path()
                    .starts_with("/microsoft/fabric-cli/releases/download/rust-v")
        }
        _ => false,
    };
    if !initial && !is_allowed_release_cdn(value) {
        return Err(Error::Origin);
    }
    Ok(url)
}

fn selected_policy(value: Option<&str>) -> Result<Policy, Failure> {
    match value {
        None | Some("off") => Ok(Policy::Off),
        Some("check") => Ok(Policy::Check),
        Some("install") => Ok(Policy::Install),
        _ => Err((
            "update_configuration",
            "FAB_RUST_UPDATE_POLICY must be off, check, or install.",
        )),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CadenceState {
    attempted_unix: u64,
}

struct CadenceLock {
    path: PathBuf,
    file: Option<File>,
    owned: bool,
}

impl CadenceLock {
    fn create(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
            owned: true,
        })
    }

    fn release(&mut self) -> io::Result<()> {
        self.file.take();
        if self.owned {
            self.owned = false;
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

impl Drop for CadenceLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn cadence_attempt() -> Result<bool, Failure> {
    let root = std::env::var_os("FAB_RUST_UPDATE_STATE_ROOT")
        .map(PathBuf::from)
        .ok_or((
            "update_configuration",
            "Automatic updates require an explicit absolute FAB_RUST_UPDATE_STATE_ROOT directory.",
        ))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            (
                "update_clock",
                "The wall clock cannot establish update cadence.",
            )
        })?
        .as_secs();
    cadence_at(&root, now)
}

fn cadence_at(root: &Path, now: u64) -> Result<bool, Failure> {
    cadence_at_with_lock_write(root, now, |file| file.write_all(b"owned-cadence-lock"))
}

fn cadence_at_with_lock_write(
    root: &Path,
    now: u64,
    write_lock: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<bool, Failure> {
    let failure = || {
        (
            "update_cadence",
            "The update cadence state is invalid or unavailable; no automatic check was started.",
        )
    };
    if !root.is_absolute() {
        return Err(failure());
    }
    for ancestor in root.ancestors() {
        if fs::symlink_metadata(ancestor)
            .map_err(|_| failure())?
            .file_type()
            .is_symlink()
        {
            return Err(failure());
        }
    }
    if !fs::metadata(root).map_err(|_| failure())?.is_dir() {
        return Err(failure());
    }
    let mut lock =
        CadenceLock::create(root.join(".fabric-update-cadence.lock")).map_err(|_| failure())?;
    let result = (|| {
        write_lock(lock.file.as_mut().ok_or_else(failure)?).map_err(|_| failure())?;
        let path = root.join("fabric-update-cadence.json");
        let previous = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024
                {
                    return Err(failure());
                }
                let mut bytes = Vec::new();
                File::open(&path)
                    .map_err(|_| failure())?
                    .take(1025)
                    .read_to_end(&mut bytes)
                    .map_err(|_| failure())?;
                if bytes.len() > 1024 {
                    return Err(failure());
                }
                Some(
                    serde_json::from_slice::<CadenceState>(&bytes)
                        .map_err(|_| failure())?
                        .attempted_unix,
                )
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Err(failure()),
        };
        let due = previous.is_none_or(|last| now >= last && now - last >= CHECK_INTERVAL);
        if due || previous.is_some_and(|last| now < last) {
            let next = root.join(".fabric-update-cadence.next");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&next)
                .map_err(|_| failure())?;
            let write = (|| {
                serde_json::to_writer(
                    &mut file,
                    &CadenceState {
                        attempted_unix: now,
                    },
                )
                .map_err(|_| failure())?;
                file.sync_all().map_err(|_| failure())
            })();
            drop(file);
            if write.is_err() {
                let _ = fs::remove_file(&next);
                return write.map(|_| due);
            }
            if fs::rename(&next, &path).is_err() {
                let _ = fs::remove_file(&next);
                return Err(failure());
            }
        }
        Ok(due)
    })();
    lock.release().map_err(|_| failure())?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use azure_core::http::{AsyncRawResponse, StatusCode, headers::Headers};
    use std::sync::Mutex;

    #[test]
    fn feature_variant_keeps_certificate_support_distinct_within_the_protocol_bound() {
        let variant = feature_variant();
        assert!(variant.len() <= 64);
        if cfg!(any(
            feature = "interactive",
            feature = "certificate-store-pkcs11",
            feature = "certificate-store-macos"
        )) {
            assert!(variant.starts_with("f2-"));
            assert_eq!(variant.len(), 63);
            return;
        }
        assert_eq!(
            variant
                .split('+')
                .any(|feature| feature == "certificate-store-windows"),
            cfg!(feature = "certificate-store-windows")
        );
        if cfg!(all(
            feature = "certificate-store-windows",
            feature = "native-read",
            feature = "repl"
        )) {
            assert_eq!(
                variant,
                "self-update+certificate-store-windows+native-read+repl"
            );
        }
    }

    fn test_root() -> PathBuf {
        let root = std::env::var_os("FAB_UPDATE_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        assert!(root.is_absolute());
        root
    }

    #[derive(Debug)]
    struct Wire {
        requests: Mutex<Vec<Request>>,
        status: u16,
        headers: Headers,
        body: Vec<u8>,
        pending: bool,
    }

    #[async_trait]
    impl HttpClient for Wire {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            self.requests.lock().unwrap().push(request.clone());
            if self.pending {
                return std::future::pending().await;
            }
            Ok(AsyncRawResponse::from_bytes(
                StatusCode::from(self.status),
                self.headers.clone(),
                self.body.clone(),
            ))
        }
    }

    #[derive(Default)]
    struct Sink(Vec<u8>);

    impl ResponseSink for Sink {
        fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }

    fn pending(max_bytes: usize) -> HttpRequest {
        HttpRequest {
            url: "https://github.com/microsoft/fabric-cli/releases/download/rust-v1.2.3/fab-rs.exe"
                .to_owned(),
            max_bytes,
            deadline_ms: 2000,
            accept: "application/octet-stream",
            api_version: "2022-11-28",
        }
    }

    fn wire(headers: Headers, body: &[u8]) -> Arc<Wire> {
        Arc::new(Wire {
            requests: Mutex::new(Vec::new()),
            status: 200,
            headers,
            body: body.to_vec(),
            pending: false,
        })
    }

    #[test]
    fn trust_policy_and_rust_origin_fail_closed_without_environment_key_fallback() {
        assert_eq!(parse_key(None).unwrap_err().0, "NotConfigured");
        assert_eq!(classify(Error::ReleaseUnavailable).0, "NotConfigured");
        for invalid in [
            "",
            "not-a-public-key",
            &"0".repeat(64),
            &"1".repeat(63),
            &"g".repeat(64),
        ] {
            assert!(parse_key(Some(invalid)).is_err());
        }
        assert_eq!(parse_key(Some(&"a1".repeat(32))).unwrap(), [0xa1; 32]);
        assert_eq!(selected_policy(None).unwrap(), Policy::Off);
        assert!(selected_policy(Some("automatic")).is_err());
        for invalid in [
            "http://github.com/microsoft/fabric-cli/releases/download/rust-v1.2.3/fab-rs.exe",
            "https://github.com/other/product/releases/download/rust-v1.2.3/fab-rs.exe",
            "https://github.com:443/microsoft/fabric-cli/releases/download/rust-v1.2.3/fab-rs.exe",
            "https://api.github.com/repos/other/product/releases/latest",
            "https://objects.githubusercontent.com.evil.invalid/asset",
            "https://user@objects.githubusercontent.com/asset",
            "https://objects.githubusercontent.com:443/asset",
        ] {
            assert_eq!(checked_url(invalid), Err(Error::Origin));
        }
    }

    #[test]
    fn actual_anonymous_adapter_forwards_no_auth_and_never_follows_redirects_itself() {
        let control = Arc::new(HostControl::new());
        let runtime = UpdateRuntime::new(control.clone(), false).unwrap();
        let mut headers = Headers::new();
        headers.insert(
            "location",
            "https://release-assets.githubusercontent.com/fixture?sig=local",
        );
        let wire = Arc::new(Wire {
            status: 302,
            headers,
            body: Vec::new(),
            requests: Mutex::new(Vec::new()),
            pending: false,
        });
        let mut transport = AnonymousTransport {
            handle: runtime.handle.clone(),
            http: wire.clone(),
        };
        let mut sink = Sink::default();
        let response = transport
            .get(&pending(8), &mut sink, control.as_ref())
            .unwrap();
        assert_eq!(response.status, 302);
        assert!(sink.0.is_empty());
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        for forbidden in [
            "authorization",
            "cookie",
            "proxy-authorization",
            "x-ms-client-request-id",
        ] {
            assert!(requests[0].headers().get_str(&forbidden.into()).is_err());
        }
        assert_eq!(
            requests[0]
                .headers()
                .get_str(&"accept-encoding".into())
                .unwrap(),
            "identity"
        );
    }

    #[test]
    fn actual_adapter_bounds_body_and_rejects_encoded_or_oversized_content() {
        let control = Arc::new(HostControl::new());
        let runtime = UpdateRuntime::new(control.clone(), false).unwrap();
        for (header, expected) in [
            (None, Error::Limit),
            (Some(("content-length", "9")), Error::Limit),
            (Some(("content-encoding", "gzip")), Error::Response),
        ] {
            let mut headers = Headers::new();
            if let Some((name, value)) = header {
                headers.insert(name, value);
            }
            let http = wire(headers, b"123456789");
            let mut transport = AnonymousTransport {
                handle: runtime.handle.clone(),
                http,
            };
            assert_eq!(
                transport
                    .get(&pending(8), &mut Sink::default(), control.as_ref())
                    .unwrap_err(),
                expected
            );
        }
        let http = wire(Headers::new(), b"1234");
        let mut transport = AnonymousTransport {
            handle: runtime.handle.clone(),
            http,
        };
        let mut sink = Sink::default();
        assert_eq!(
            transport
                .get(&pending(8), &mut sink, control.as_ref())
                .unwrap()
                .status,
            200
        );
        assert_eq!(sink.0, b"1234");
    }

    #[test]
    fn actual_adapter_cancels_pending_io_at_the_overall_deadline() {
        let control = Arc::new(HostControl::new());
        let runtime = UpdateRuntime::new(control.clone(), false).unwrap();
        let http = Arc::new(Wire {
            pending: true,
            requests: Mutex::new(Vec::new()),
            status: 200,
            headers: Headers::new(),
            body: Vec::new(),
        });
        let mut transport = AnonymousTransport {
            handle: runtime.handle.clone(),
            http,
        };
        let mut request = pending(8);
        request.deadline_ms = control.snapshot().monotonic_ms + 100;
        assert_eq!(
            transport
                .get(&request, &mut Sink::default(), control.as_ref())
                .unwrap_err(),
            Error::Deadline
        );
        control.cancelled.store(true, Ordering::Release);
        assert_eq!(
            transport
                .get(&pending(8), &mut Sink::default(), control.as_ref())
                .unwrap_err(),
            Error::Cancelled
        );
    }

    #[test]
    fn help_flags_named_version_and_batch_are_excluded_by_pure_classification() {
        for arguments in [
            vec!["fab-rs", "--help"],
            vec!["fab-rs", "-h"],
            vec!["fab-rs", "--version"],
            vec!["fab-rs", "-V"],
            vec!["fab-rs", "version"],
            vec!["fab-rs", "-c", "version"],
            vec!["fab-rs", "update", "--check"],
        ] {
            let invocation = fabric_cli::parser::parse_args(arguments).unwrap();
            assert!(!startup_invocation_allowed(&invocation));
        }
        let version = fabric_cli::parser::parse_args(["fab-rs", "version"]).unwrap();
        assert!(matches!(version, Invocation::Local(ref local)
            if matches!(&local.command, fabric_cli_local::LocalCommand::Version)));
        let read = fabric_cli::parser::parse_args(["fab-rs", "ls", "/"]).unwrap();
        assert!(startup_invocation_allowed(&read));
    }

    #[cfg(feature = "windows-broker")]
    #[test]
    fn explicit_auth_is_excluded_before_any_update_component_setup() {
        let invocation = fabric_cli::parser::parse_args([
            "fab-rs",
            "auth",
            "status",
            "--tenant",
            "11111111-1111-1111-1111-111111111111",
            "--account",
            "fixture@example.invalid",
        ])
        .unwrap();
        assert!(!startup_invocation_allowed(&invocation));
    }

    #[test]
    fn persisted_cadence_handles_failures_clock_skew_and_corruption_without_network() {
        let root = test_root();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = root.join(format!(
            "cli-update-cadence-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        assert!(cadence_at(&directory, 1000).unwrap());
        assert!(!cadence_at(&directory, 1001).unwrap());
        assert!(cadence_at(&directory, 1000 + CHECK_INTERVAL).unwrap());
        assert!(!cadence_at(&directory, 100).unwrap());
        assert!(!cadence_at(&directory, 101).unwrap());
        let state = directory.join("fabric-update-cadence.json");
        fs::write(&state, br#"{"attempted_unix":1,"attempted_unix":2}"#).unwrap();
        assert!(cadence_at(&directory, 1000).is_err());
        let lock = directory.join(".fabric-update-cadence.lock");
        fs::write(&lock, b"other-owner").unwrap();
        assert!(cadence_at(&directory, 1000).is_err());
        assert_eq!(fs::read(&lock).unwrap(), b"other-owner");
        fs::remove_file(lock).unwrap();
        fs::remove_file(state).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cadence_post_create_write_failure_releases_owned_lock_and_retry_succeeds() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = test_root().join(format!(
            "cli-cadence-write-fault-{:x}-{unique:x}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let lock = directory.join(".fabric-update-cadence.lock");
        let state = directory.join("fabric-update-cadence.json");
        let failure = cadence_at_with_lock_write(&directory, 1000, |file| {
            assert!(lock.exists());
            file.write_all(b"partial-lock")?;
            Err(io::Error::other("synthetic post-create write failure"))
        });
        assert!(matches!(failure, Err(("update_cadence", _))));
        assert!(!lock.exists());
        assert!(!state.exists());
        assert!(!directory.join(".fabric-update-cadence.next").exists());
        assert!(cadence_at(&directory, 1000).unwrap());
        assert!(!lock.exists());
        assert!(!cadence_at(&directory, 1001).unwrap());
        fs::write(&lock, b"foreign-lock").unwrap();
        assert!(
            cadence_at_with_lock_write(&directory, 1002, |_| {
                panic!("a pre-existing foreign lock must not reach writing");
            })
            .is_err()
        );
        assert_eq!(fs::read(&lock).unwrap(), b"foreign-lock");
        fs::remove_file(lock).unwrap();
        fs::remove_file(state).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn normal_automatic_failures_never_replace_the_business_exit() {
        for code in [
            "NotConfigured",
            "update_transport",
            "update_timeout",
            "update_custody",
            "update_cadence",
            "update_signature",
            "update_integrity",
            "update_expired",
            "update_binding",
            "update_schema",
            "update_native_context",
            "update_major_opt_in_required",
        ] {
            assert!(startup_failure_outcome(code, false).is_none());
        }
        assert!(matches!(
            startup_failure_outcome("update_recovery_required", false),
            Some(CommandOutcome::Stop(1))
        ));
    }

    #[test]
    fn successful_automatic_apply_is_rerun_required_not_business_success() {
        assert!(startup_success_outcome(UpdateAction::Check, false).is_none());
        assert!(matches!(
            startup_success_outcome(UpdateAction::Apply, false),
            Some(CommandOutcome::Stop(75))
        ));
        let report = json!({"state":"pending_parent_exit","installed":false,"exit_required":true});
        let notice = startup_success_notice(&report, UpdateAction::Apply);
        assert_eq!(notice["requested_command_executed"], false);
        assert_eq!(notice["rerun_required"], true);
        assert_eq!(notice["exit_status"], 75);
        assert_eq!(notice["update_notice"]["installed"], false);
        assert_eq!(notice["code"], "update_exit_rerun_required");
        let check_notice =
            startup_success_notice(&json!({"state":"available"}), UpdateAction::Check);
        assert!(check_notice.get("rerun_required").is_none());
    }

    #[test]
    fn authoritative_pending_lifetime_forces_exit_even_for_unclassified_postready_errors() {
        for code in [
            "update_schema",
            "update_filesystem",
            "future_unclassified_failure",
        ] {
            assert!(matches!(
                startup_failure_outcome(code, true),
                Some(CommandOutcome::Stop(1))
            ));
            assert!(startup_failure_outcome(code, false).is_none());
        }
        assert!(matches!(
            startup_success_outcome(UpdateAction::Check, true),
            Some(CommandOutcome::Stop(75))
        ));
        assert!(matches!(
            explicit_outcome(UpdateAction::Check, 1, true),
            CommandOutcome::Stop(1)
        ));
        assert!(matches!(
            explicit_outcome(UpdateAction::Check, 0, true),
            CommandOutcome::Stop(0)
        ));
        assert!(matches!(
            explicit_outcome(UpdateAction::Check, 1, false),
            CommandOutcome::Returned(1)
        ));
    }

    #[test]
    fn automatic_install_never_opts_into_a_major_upgrade() {
        let automatic = automatic_command(StartupDecision::CheckAndInstall);
        assert_eq!(automatic.action, UpdateAction::Apply);
        assert!(!automatic.allow_major);
        assert!(matches!(
            require_major_permission(&automatic, true),
            Err(Error::MajorUpgradeRequiresOptIn)
        ));
        assert!(startup_failure_outcome("update_major_opt_in_required", false).is_none());
        let explicit = UpdateCommand {
            action: UpdateAction::Apply,
            tag: Some("rust-v2.0.0".to_owned()),
            allow_major: false,
        };
        assert!(matches!(
            require_major_permission(&explicit, true),
            Err(Error::MajorUpgradeRequiresOptIn)
        ));
        let opted_in = UpdateCommand {
            allow_major: true,
            ..explicit
        };
        assert!(require_major_permission(&opted_in, true).is_ok());
        let check = automatic_command(StartupDecision::Check);
        assert!(require_major_permission(&check, true).is_ok());
        assert!(matches!(
            require_major_permission(
                &UpdateCommand {
                    allow_major: true,
                    ..check
                },
                true
            ),
            Err(Error::InvalidIdentity)
        ));
    }

    #[test]
    fn standalone_custody_requires_an_exact_installer_receipt_not_writability() {
        let root = test_root();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = root.join(format!("cli-custody-{:x}-{unique:x}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let executable = directory.join("f.exe");
        fs::write(&executable, b"local-installer-receipt-fixture").unwrap();
        let helper = directory.join(fabric_cli_update::helper_name());
        fs::write(&helper, b"trusted-installer-helper-fixture").unwrap();
        assert!(!fs::metadata(&executable).unwrap().permissions().readonly());
        let receipt_path = directory.join(".fab-rs-install.json");
        let control = HostControl::new();
        let target = Target::WindowsX64;
        assert!(
            load_install_receipt(&receipt_path, &executable, "self-update", target, &control)
                .is_err()
        );
        let current = hash_current(&executable, &control).unwrap();
        let digest = current
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let receipt = json!({
            "schema":2,
            "product":"fab-rs",
            "repository":"microsoft/fabric-cli",
            "custody":"standalone",
            "executable":executable,
            "version":env!("CARGO_PKG_VERSION"),
            "target":target.as_str(),
            "variant":"self-update",
            "sha256":digest,
            "helper":helper,
            "helper_sha256":hash_current(&helper, &control).unwrap().iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "backups":[]
        });
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let bound =
            load_install_receipt(&receipt_path, &executable, "self-update", target, &control)
                .unwrap();
        assert_eq!(bound.current_sha256, current);
        for field in [
            "schema",
            "product",
            "repository",
            "custody",
            "executable",
            "version",
            "target",
            "variant",
            "sha256",
            "helper",
            "helper_sha256",
            "backups",
        ] {
            let mut bad = receipt.clone();
            bad[field] = match field {
                "schema" => json!(1),
                "custody" => json!("package-manager"),
                "executable" => json!(directory.join("other.exe")),
                "sha256" => json!("0".repeat(64)),
                "helper" => json!(directory.join("untrusted-helper.exe")),
                "helper_sha256" => json!("0".repeat(64)),
                "backups" => json!([{}]),
                _ => json!("not-this-install"),
            };
            fs::write(&receipt_path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(
                matches!(
                    load_install_receipt(&receipt_path, &executable, "self-update", target, &control),
                    Err((code, _)) if code == "update_custody"
                ),
                "{field}"
            );
        }
        let text = serde_json::to_string(&receipt).unwrap();
        for invalid in [
            text.replacen('{', "{\"schema\":2,", 1),
            text.replacen('{', "{\"unknown\":0,", 1),
        ] {
            fs::write(&receipt_path, invalid).unwrap();
            assert!(
                load_install_receipt(&receipt_path, &executable, "self-update", target, &control)
                    .is_err()
            );
        }
        fs::remove_file(receipt_path).unwrap();
        fs::remove_file(executable).unwrap();
        fs::remove_file(helper).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
