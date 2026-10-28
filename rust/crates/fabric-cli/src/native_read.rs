// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit opt-in binary composition. Libraries never read these host inputs.

use fabric_cli::{
    HostError,
    parser::{Invocation, JobMutationCommand, JobReadCommand, JobRunCommand, ReadCommand},
};
use fabric_sdk::{EndpointBinding, ServiceOrigin};
use serde_json::json;
use std::{
    io::{self, Write},
    process::ExitCode,
};

type Failure = (&'static str, String, u8);

#[cfg(windows)]
#[path = "native_settings.rs"]
mod settings;

#[derive(Clone, Copy)]
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "The selected host is explicitly unsupported off Windows."
    )
)]
enum NativeCommand<'a> {
    Read(&'a ReadCommand),
    Job(&'a JobReadCommand),
    JobExecution(&'a JobRunCommand),
    JobMutation(&'a JobMutationCommand),
    Family(&'a Invocation),
}

fn failure(code: &'static str, message: impl Into<String>, exit: u8) -> Failure {
    (code, message.into(), exit)
}

#[cfg(any(windows, test))]
fn interrupted_failure(code: &'static str, message: &str, exit: u8) -> Failure {
    failure(
        code,
        format!(
            "{message} Local waiting has stopped; in-flight remote or filesystem work may still complete. No rollback is implied."
        ),
        exit,
    )
}

#[cfg(any(windows, test))]
fn finish_runtime<T>(runtime: tokio::runtime::Runtime, result: T) -> T {
    // Runtime Drop waits indefinitely for blocking syscalls; a bounded wait does
    // not cancel those calls or erase their possible filesystem effects.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

fn write_error((code, message, exit): Failure) -> io::Result<ExitCode> {
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(
        &mut stderr,
        &json!({"error": {"code": code, "message": message}}),
    )
    .map_err(io::Error::other)?;
    writeln!(stderr)?;
    Ok(ExitCode::from(exit))
}

fn required(name: &str) -> Result<String, Failure> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Err(std::env::VarError::NotPresent) => Err(failure(
            "NotConfigured",
            HostError::NotConfigured.to_string(),
            1,
        )),
        _ => Err(failure(
            "invalid_configuration",
            "Invalid native read selector.",
            2,
        )),
    }
}

fn selected_endpoint(value: Option<std::ffi::OsString>) -> Result<EndpointBinding, Failure> {
    match value {
        None => Ok(EndpointBinding::public(ServiceOrigin::Fabric)),
        Some(value) => value
            .to_str()
            .and_then(|host| EndpointBinding::with_host(ServiceOrigin::Fabric, host).ok())
            .ok_or_else(|| {
                failure(
                    "invalid_configuration",
                    "Invalid Fabric endpoint hostname.",
                    2,
                )
            }),
    }
}

pub(crate) fn run(command: &ReadCommand) -> io::Result<ExitCode> {
    run_command(NativeCommand::Read(command))
}

pub(crate) fn run_job(command: &JobReadCommand) -> io::Result<ExitCode> {
    run_command(NativeCommand::Job(command))
}

pub(crate) fn run_job_execution(command: &JobRunCommand) -> io::Result<ExitCode> {
    run_command(NativeCommand::JobExecution(command))
}

pub(crate) fn run_job_mutation(command: &JobMutationCommand) -> io::Result<ExitCode> {
    run_command(NativeCommand::JobMutation(command))
}

pub(crate) fn run_family(command: &Invocation) -> io::Result<ExitCode> {
    run_command(NativeCommand::Family(command))
}

fn run_command(command: NativeCommand<'_>) -> io::Result<ExitCode> {
    let result = (|| {
        // Public selectors only, never token/secret environment inputs.
        let endpoint = selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
        if endpoint != EndpointBinding::public(ServiceOrigin::Fabric)
            && !matches!(command, NativeCommand::Read(_))
        {
            return Err(failure(
                "NotSupported",
                "Non-public Fabric endpoints currently support only ls and get.",
                1,
            ));
        }
        let tenant = required("FAB_RUST_TENANT")?;
        let account = required("FAB_RUST_ACCOUNT")?;
        let client = required("FAB_RUST_CLIENT_ID")?;
        run_selected(command, &tenant, &account, &client, endpoint)
    })();
    match result {
        Ok(plan) => {
            plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())?;
            Ok(ExitCode::from(plan.exit_code))
        }
        Err(error) => write_error(error),
    }
}

#[cfg(not(windows))]
fn run_selected(
    _: NativeCommand<'_>,
    _: &str,
    _: &str,
    _: &str,
    _: EndpointBinding,
) -> Result<fabric_cli_output::text::TextPlan, Failure> {
    Err(failure(
        "unsupported",
        "Native read credentials currently require Windows.",
        1,
    ))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn default_public_daily_and_msit_keep_one_fabric_scope() {
        assert_eq!(
            selected_endpoint(None).unwrap(),
            EndpointBinding::public(ServiceOrigin::Fabric)
        );
        for host in [
            "api.fabric.microsoft.com",
            "dailyapi.fabric.microsoft.com",
            "msitapi.fabric.microsoft.com",
        ] {
            let endpoint = selected_endpoint(Some(host.into())).unwrap();
            assert_eq!(endpoint.host(), host);
            assert_eq!(endpoint.origin(), ServiceOrigin::Fabric);
            assert_eq!(
                endpoint.audience().scope(),
                "https://api.fabric.microsoft.com/.default"
            );
        }
    }

    #[test]
    fn invalid_endpoint_inputs_fail_without_echoing_the_host() {
        for host in [
            "",
            "https://msitapi.fabric.microsoft.com",
            "api.fabric.microsoft.com:443",
            "api.fabric.microsoft.com/path",
            "api.fabric.microsoft.com.example.test",
            "unknown.example.test",
            "user@api.fabric.microsoft.com",
        ] {
            let (code, message, exit) = selected_endpoint(Some(host.into())).unwrap_err();
            assert_eq!(code, "invalid_configuration");
            assert_eq!(message, "Invalid Fabric endpoint hostname.");
            assert_eq!(exit, 2);
        }
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    #[test]
    fn shutdown_preserves_the_original_success_or_typed_failure() {
        for result in [
            Ok(7_u8),
            Err(failure("synthetic_failed", "Synthetic failure.", 4)),
        ] {
            assert_eq!(finish_runtime(runtime(), result.clone()), result);
        }
    }

    #[test]
    fn shutdown_is_bounded_while_blocking_work_can_still_finish_with_effects() {
        let runtime = runtime();
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let effect = Arc::new(AtomicBool::new(false));
        let worker_effect = Arc::clone(&effect);
        let _worker = runtime.spawn_blocking(move || {
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(30)).unwrap();
            worker_effect.store(true, Ordering::SeqCst);
            completed.send(()).unwrap();
        });
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        let (returned, result) = mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            returned
                .send(finish_runtime(
                    runtime,
                    Err::<u8, _>(interrupted_failure("cancelled", "Read cancelled.", 2)),
                ))
                .unwrap();
        });

        // Release before asserting, so a regression to ordinary runtime Drop
        // fails the test without stranding its blocking worker.
        let observed = result.recv_timeout(Duration::from_secs(10));
        let effect_before_release = effect.load(Ordering::SeqCst);
        release.send(()).unwrap();
        completion.recv_timeout(Duration::from_secs(10)).unwrap();
        shutdown.join().unwrap();

        assert_eq!(
            observed.unwrap(),
            Err(interrupted_failure("cancelled", "Read cancelled.", 2))
        );
        assert!(!effect_before_release);
        assert!(effect.load(Ordering::SeqCst));
    }

    #[test]
    fn interruption_diagnostics_retain_code_exit_and_mutation_uncertainty() {
        for (code, message, exit) in [
            ("cancelled", "Read cancelled.", 2),
            ("timed_out", "The read command deadline elapsed.", 1),
        ] {
            let (actual_code, actual_message, actual_exit) =
                interrupted_failure(code, message, exit);
            assert_eq!(actual_code, code);
            assert_eq!(actual_exit, exit);
            assert!(actual_message.starts_with(message));
            assert!(actual_message.contains("may still complete"));
            assert!(actual_message.contains("No rollback is implied"));
        }
    }
}

#[cfg(windows)]
fn run_selected(
    command: NativeCommand<'_>,
    tenant: &str,
    account: &str,
    client: &str,
    endpoint: EndpointBinding,
) -> Result<fabric_cli_output::text::TextPlan, Failure> {
    let options = fabric_identity::LoginOptions::new(tenant, client, account)
        .map_err(|e| failure("invalid_configuration", e.to_string(), 2))?;
    let (current, restored) = crate::local_host::context_selection()
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    let home = std::env::var("FAB_RUST_HOME").map_or_else(
        |e| match e {
            std::env::VarError::NotPresent => Ok(fabric_cli_context::HomeContext::Unspecified),
            _ => Err(failure(
                "invalid_configuration",
                "Invalid home selector.",
                2,
            )),
        },
        selected_home,
    )?;
    if let NativeCommand::Read(command) = command
        && (command.output.is_some() || command.force || command.verbose)
    {
        return Err(failure(
            "NotSupported",
            "GET export, verbose and definition/confirmation options are not ported.",
            1,
        ));
    }
    let skill = match command {
        NativeCommand::Read(_) => None,
        NativeCommand::Job(command) => command.skill.as_deref(),
        NativeCommand::JobExecution(command) => command.skill.as_deref(),
        NativeCommand::JobMutation(command) => command.skill.as_deref(),
        NativeCommand::Family(_) => None,
    };
    fabric_sdk_jobs::JobRequestHeaders::with_skill(skill).map_err(|_| {
        failure(
            "invalid_arguments",
            "Invalid job request header selection.",
            2,
        )
    })?;
    let saved = if std::env::var_os("FAB_RUST_CONFIG_ROOT").is_some() {
        Some(
            crate::local_host::store()
                .and_then(|store| store.initialize())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?,
        )
    } else {
        None
    };
    let settings = settings::NativeSettings::select(saved.as_ref())
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    let job = match command {
        NativeCommand::JobExecution(command) => Some(command.family_command()),
        NativeCommand::JobMutation(command) => Some(command.family_command()),
        _ => None,
    };
    let deadline = settings::command_deadline(job.as_ref(), settings.jobs)
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    if std::time::Instant::now().checked_add(deadline).is_none() {
        return Err(failure(
            "invalid_configuration",
            "The selected job timeout exceeds the native deadline range.",
            2,
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failure("runtime_failed", "The read runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                match signal {
                    Ok(()) => Err(interrupted_failure("cancelled", "Read cancelled.", 2)),
                    Err(_) => Err(failure("signal_failed", "The cancellation handler could not start.", 1)),
                }
            },
            _ = tokio::time::sleep(deadline) => {
                Err(interrupted_failure("timed_out", "The read command deadline elapsed.", 1))
            },
            result = execute(command, options, &current, home, restored, endpoint, settings) => result,
        }
    });
    finish_runtime(runtime, result)
}

#[cfg(windows)]
fn selected_home(value: String) -> Result<fabric_cli_context::HomeContext, Failure> {
    let valid = value.rsplit_once('.').is_some_and(|(name, suffix)| {
        !name.is_empty()
            && !name.contains(['\n', '\r'])
            && (suffix.eq_ignore_ascii_case("Workspace") || suffix.eq_ignore_ascii_case("Personal"))
    });
    if !valid {
        return Err(failure(
            "invalid_configuration",
            "Invalid home selector.",
            2,
        ));
    }
    Ok(fabric_cli_context::HomeContext::Workspace(value))
}

#[cfg(windows)]
async fn execute(
    command: NativeCommand<'_>,
    options: fabric_identity::LoginOptions,
    current: &str,
    home: fabric_cli_context::HomeContext,
    restored: bool,
    endpoint: EndpointBinding,
    settings: settings::NativeSettings,
) -> Result<fabric_cli_output::text::TextPlan, Failure> {
    use fabric_cli::session::ReadSession;
    use fabric_cli_context::Element;
    use fabric_sdk::{Audience, Client, CredentialSelection, RequestContext};
    use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
    use fabric_sdk_transport::{
        NativeHttpClient, ProxyPolicy, TokioTimer, TransportOptions, TrustPolicy,
    };
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};

    let selection = CredentialSelection::new(options.account(), options.tenant(), Audience::Fabric)
        .map_err(|_| failure("invalid_configuration", "Invalid credential selection.", 2))?;
    let tenant = Element::tenant(options.tenant(), options.tenant());
    let credential = fabric_identity::WindowsCredential::connect(options)
        .await
        .map_err(|error| {
            let (code, message) = crate::auth::broker_error(error);
            failure(code, message, 1)
        })?;
    let sdk = Client::with_context(RequestContext::with_endpoint(
        credential,
        selection,
        endpoint.clone(),
    ));
    let client = NativeHttpClient::new(TransportOptions {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(30),
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(4).expect("nonzero constant"),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    })
    .map_err(|_| {
        failure(
            "transport_failed",
            "The native HTTP client could not start.",
            1,
        )
    })?;
    let executor = HttpExecutor::new(
        Arc::new(client),
        Arc::new(TokioTimer),
        ExecutionOptions::default(),
    )
    .map_err(|_| failure("transport_failed", "The HTTP executor could not start.", 1))?
    .with_endpoint(endpoint);
    let now = azure_core::time::OffsetDateTime::now_utc();
    let timestamp = fabric_cli_output::Timestamp::parse(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.microsecond(),
    ))
    .map_err(|_| {
        failure(
            "clock_failed",
            "The UTC clock is outside the supported timestamp range.",
            1,
        )
    })?;
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant,
        current_path: current,
        home,
        timestamp,
        config: settings.reads,
    };
    let result = match command {
        NativeCommand::Read(command) => {
            let command = settings
                .read_command(command)
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            session
                .with_source_operations(&executor)
                .dispatch(&command, std::future::pending())
                .await
        }
        NativeCommand::Job(command) => {
            let mut command = command.clone();
            command.output_format = Some(
                settings
                    .output(command.output_format.as_deref())
                    .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?,
            );
            fabric_cli::jobs::JobReadSession {
                reads: &session,
                jobs: Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
            }
            .dispatch(&command, std::future::pending())
            .await
        }
        NativeCommand::JobExecution(command) => {
            let command = settings
                .job_command(&command.family_command())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            execute_job(&session, &executor, &command, settings.jobs).await
        }
        NativeCommand::JobMutation(command) => {
            let command = settings
                .job_command(&command.family_command())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            execute_job(&session, &executor, &command, settings.jobs).await
        }
        NativeCommand::Family(command) => execute_family(&session, &executor, command).await,
    };
    match result {
        Ok(mut plan) => {
            if restored {
                use fabric_cli_output::text::{self, Presentation};
                let message = format!(
                    "Command context path: {}",
                    if current == "/" { "root" } else { current }
                );
                let mut warning = text::render_notice(
                    text::Notice::Warning(&message),
                    None,
                    Presentation::default(),
                );
                warning.records.append(&mut plan.records);
                plan.records = warning.records;
            }
            Ok(plan)
        }
        Err(error) => {
            // Retain source output-before-error, rather than silently dropping it.
            if let Some(partial) = error.partial_output() {
                partial
                    .write_to(&mut io::stdout().lock(), &mut io::stderr().lock())
                    .map_err(|_| {
                        failure("output_failed", "Writing partial read output failed.", 1)
                    })?;
            }
            Err(failure("render_failed", error.to_string(), 1))
        }
    }
}

#[cfg(windows)]
async fn execute_job(
    session: &fabric_cli::session::ReadSession<'_>,
    executor: &fabric_sdk_http::HttpExecutor,
    command: &fabric_cli_jobs::JobCommand,
    config: fabric_cli_jobs::JobConfig,
) -> Result<fabric_cli_output::text::TextPlan, fabric_cli_output::text::TextError> {
    fabric_cli::jobs::JobSession {
        reads: session,
        jobs: std::sync::Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
        files: &fabric_cli::jobs::SelectedJobFiles,
        ui: &NativeJobUi,
        config,
    }
    .dispatch(command, std::future::pending())
    .await
}

#[cfg(windows)]
async fn execute_family(
    session: &fabric_cli::session::ReadSession<'_>,
    executor: &fabric_sdk_http::HttpExecutor,
    command: &Invocation,
) -> Result<fabric_cli_output::text::TextPlan, fabric_cli_output::text::TextError> {
    use fabric_cli_context::PathElement;
    use fabric_cli_local::NavigationState;
    use fabric_cli_mutations::frontdoor::ResourceCommand;
    use fabric_cli_output::text::{self, OutputFormat, Presentation, TextOptions};

    let result = async {
        let store = crate::local_host::store()?;
        store.initialize()?;
        let local_root = std::env::current_dir()?;
        let mut navigation = NavigationState::new(
            PathElement::from_fabric(session.tenant.clone()),
            session.home.clone(),
            local_root.to_string_lossy().into_owned(),
            crate::local_host::session_id()?,
            true,
        )?;
        let mut effects = NativeLocalEffects {
            base: crate::local_host::Effects,
            identity: fabric_cli_local::BrowserIdentity {
                user: true,
                claims: vec![
                    ("upn".into(), session.sdk.selection().account().into()),
                    ("tid".into(), session.sdk.selection().tenant().into()),
                ],
            },
            capacities: None,
        };
        if matches!(command, Invocation::Local(local)
                if matches!(&local.command, fabric_cli_local::LocalCommand::ConfigSet { key, .. }
                    if key.eq_ignore_ascii_case("default_capacity")))
        {
            let list = session
                .sdk
                .list_capacities(session.executor, Default::default(), std::future::pending())
                .await
                .map_err(|_| {
                    fabric_cli_local::LocalError::command(
                        "ReadFailed",
                        "Capacity metadata did not complete",
                    )
                })?;
            effects.capacities = Some(
                list.value
                    .into_iter()
                    .map(|capacity| fabric_cli_local::Capacity {
                        display_name: capacity.display_name,
                        id: capacity.id,
                    })
                    .collect(),
            );
        }
        if matches!(
            command,
            Invocation::Resource(
                ResourceCommand::Remove { force: false, .. }
                    | ResourceCommand::Set { force: false, .. }
                    | ResourceCommand::Link { force: false, .. }
                    | ResourceCommand::Start { force: false, .. }
                    | ResourceCommand::Stop { force: false, .. }
                    | ResourceCommand::Assign { force: false, .. }
                    | ResourceCommand::Unassign { force: false, .. }
                    | ResourceCommand::Move { force: false, .. }
            )
        ) {
            return Err(fabric_cli_local::LocalError::command(
                "NotSupported",
                "Native resource confirmation requires an injected UI; use --force explicitly",
            ));
        }
        let mut mutations = NativeMutationUi::default();
        let mut files = fabric_cli::families::transfer_files::TokioTransferFiles::new(
            local_root,
            None,
            Default::default(),
            tokio::runtime::Handle::current(),
        )
        .await?;
        let mut families = fabric_cli::families::FamilySession {
            reads: session,
            http: executor,
            store: &store,
            navigation: &mut navigation,
            local: &mut effects,
            mutations: &mut mutations,
            confirmation: None,
            transfer_ui: &NativeTransferUi,
            transfer_files: Some(&mut files),
            resource_files: None,
        };
        let mut output = families.dispatch(command, std::future::pending()).await?;
        let mut notices = std::mem::take(&mut mutations.records);
        notices.append(&mut output.plan.records);
        output.plan.records = notices;
        Ok::<_, fabric_cli_local::LocalError>(output.plan)
    }
    .await;
    match result {
        Ok(plan) => Ok(plan),
        Err(fabric_cli_local::LocalError::Text(error)) => Err(error),
        Err(error) => text::render(
            &error
                .command_error()
                .envelope(session.timestamp.clone(), None),
            OutputFormat::Json,
            &TextOptions::default(),
            &text::ascii_width,
            Presentation::default(),
        ),
    }
}

#[cfg(windows)]
struct NativeLocalEffects {
    base: crate::local_host::Effects,
    identity: fabric_cli_local::BrowserIdentity,
    capacities: Option<Vec<fabric_cli_local::Capacity>>,
}

#[cfg(windows)]
impl fabric_cli_local::LocalEffects for NativeLocalEffects {
    fn clear_cli_cache(&mut self) -> Result<(), fabric_cli_local::LocalError> {
        self.base.clear_cli_cache()
    }
    fn capacities(
        &mut self,
    ) -> Result<Vec<fabric_cli_local::Capacity>, fabric_cli_local::LocalError> {
        self.capacities.clone().ok_or_else(|| {
            fabric_cli_local::LocalError::command(
                "NotConfigured",
                "Capacity metadata is not loaded",
            )
        })
    }
    fn browser_identity(
        &self,
    ) -> Result<fabric_cli_local::BrowserIdentity, fabric_cli_local::LocalError> {
        Ok(self.identity.clone())
    }
    fn open_browser(&mut self, url: &str) -> Result<(), fabric_cli_local::LocalError> {
        self.base.open_browser(url)
    }
    fn clear_terminal(&mut self) -> Result<(), fabric_cli_local::LocalError> {
        self.base.clear_terminal()
    }
}

#[cfg(windows)]
#[derive(Default)]
struct NativeMutationUi {
    records: Vec<fabric_cli_output::RenderPlan>,
}

#[cfg(windows)]
impl fabric_cli_mutations::MutationUi for NativeMutationUi {
    fn notice(&mut self, kind: fabric_cli_mutations::NoticeKind, message: &str) {
        use fabric_cli_output::{
            OutputStream,
            text::{self, Presentation, Style},
        };
        let plan = match kind {
            fabric_cli_mutations::NoticeKind::Warning => text::render_notice(
                text::Notice::Warning(message),
                None,
                Presentation::default(),
            ),
            _ => text::render_line(
                message,
                Style::Grey,
                OutputStream::Stderr,
                Presentation::default(),
            ),
        };
        self.records.extend(plan.records);
    }
    fn confirm<'a>(&'a mut self, _: &'a str) -> fabric_cli_mutations::CommandFuture<'a, bool> {
        Box::pin(async { false })
    }
}

#[cfg(windows)]
struct NativeTransferUi;

#[cfg(windows)]
impl fabric_cli_transfer::TransferUi for NativeTransferUi {
    fn emit(
        &self,
        event: fabric_cli_transfer::TransferEvent,
    ) -> Result<(), fabric_cli_output::CommandError> {
        use fabric_cli_output::{
            CommandError, OutputStream,
            text::{self, Presentation, Style},
        };
        let plan = match event {
            fabric_cli_transfer::TransferEvent::Grey(message) => text::render_line(
                &message,
                Style::Grey,
                OutputStream::Stderr,
                Presentation::default(),
            ),
            fabric_cli_transfer::TransferEvent::Warning(message) => text::render_notice(
                text::Notice::Warning(&message),
                None,
                Presentation::default(),
            ),
        };
        plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())
            .map_err(|error| {
                CommandError::new(Some(error.to_string()), Some("LocalIOError".into()))
            })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<bool, fabric_cli_output::CommandError>>
    {
        Box::pin(async {
            Err(fabric_cli_output::CommandError::new(
                Some(
                    "Native transfer confirmation requires an injected UI; use --force explicitly"
                        .into(),
                ),
                Some("NotSupported".into()),
            ))
        })
    }
    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<Vec<String>, fabric_cli_output::CommandError>>
    {
        Box::pin(async {
            Err(fabric_cli_output::CommandError::new(
                Some(
                    "Native export selection requires an injected UI; use --all explicitly".into(),
                ),
                Some("NotSupported".into()),
            ))
        })
    }
}
#[cfg(windows)]
struct NativeJobUi;

#[cfg(windows)]
impl fabric_cli_jobs::JobUi for NativeJobUi {
    fn emit(
        &self,
        event: fabric_cli_jobs::JobEvent,
    ) -> Result<(), fabric_cli_output::CommandError> {
        let plan = fabric_cli::jobs::render_job_event(&event).map_err(|error| {
            fabric_cli_output::CommandError::new(
                Some(error.to_string()),
                Some("UnexpectedError".into()),
            )
        })?;
        plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())
            .map_err(|_| {
                fabric_cli_output::CommandError::new(
                    Some("Writing job output failed".into()),
                    Some("UnexpectedError".into()),
                )
            })
    }

    fn confirm(&self) -> fabric_cli_jobs::Confirmation<'_> {
        Box::pin(async {
            Err(fabric_cli_output::CommandError::new(
                Some("Native interactive confirmation is not supported; select --force explicitly or inject a confirmation UI".into()),
                Some("NotSupported".into()),
            ))
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn home_selection_preserves_resolver_escaping_and_suffix_case() {
        for name in ["A\\/B.Workspace", "Demo.workspace", "Demo.Personal"] {
            assert!(matches!(
                selected_home(name.into()).unwrap(),
                fabric_cli_context::HomeContext::Workspace(value) if value == name
            ));
        }
        for name in ["", "Demo", ".Workspace", "Demo.Notebook"] {
            assert_eq!(
                selected_home(name.into()).unwrap_err().0,
                "invalid_configuration"
            );
        }
    }
}
