// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit opt-in binary composition. Libraries never read these host inputs.

use crate::batch_state::{BatchState, CommandOutcome};
#[cfg(not(all(windows, feature = "windows-broker")))]
use fabric_cli::HostError;
use fabric_cli::parser::{
    Invocation, JobMutationCommand, JobReadCommand, JobRunCommand, ReadCommand,
};
use fabric_sdk::{EndpointBinding, ServiceOrigin};
use serde_json::json;
use std::io::{self, Write};

pub(crate) type Failure = (&'static str, String, u8);

#[cfg(feature = "workspace-read-v2")]
#[allow(
    dead_code,
    reason = "The separate workspace preview selects this entry point."
)]
pub(crate) fn run_workspace_v2(
    command: &fabric_cli::workspace_v2::WorkspaceCommand,
) -> io::Result<u8> {
    workspace_v2_native::run(command)
}

#[path = "native_get.rs"]
mod get;

#[path = "native_delete.rs"]
mod delete;

#[cfg(all(feature = "v2-run", any(windows, feature = "headless")))]
#[path = "native_run_notebook.rs"]
mod run_notebook;

#[cfg(feature = "v2-run")]
pub(crate) fn run_notebook(
    command: &fabric_cli::native_tabular::NativeCommand,
) -> io::Result<CommandOutcome> {
    #[cfg(any(windows, feature = "headless"))]
    {
        run_notebook::run(command)
    }
    #[cfg(not(any(windows, feature = "headless")))]
    {
        let _ = command;
        write_error(failure(
            "UnsupportedCapability",
            "Notebook Run requires an explicitly supported native identity host.",
            1,
        ))
    }
}

pub(crate) fn run_delete(
    prepared: fabric_cli::delete::PreparedDelete,
    output: fabric_cli::delete::ReservedDeleteOutput,
    deadline: std::time::Instant,
) -> io::Result<CommandOutcome> {
    delete::run(prepared, output, deadline)
}
#[cfg(any(windows, feature = "headless"))]
#[path = "native_execute.rs"]
mod execute;

#[cfg(all(test, feature = "headless"))]
#[path = "headless_host_tests.rs"]
mod headless_host_tests;

#[cfg(all(test, feature = "v2-cancel", feature = "headless"))]
#[path = "native_cancel_tests.rs"]
mod native_cancel_tests;

#[cfg(all(test, feature = "workspace-read-v2"))]
#[path = "native_workspace_v2_tests.rs"]
mod workspace_v2_tests;

#[cfg(feature = "workspace-read-v2")]
#[path = "native_workspace_v2.rs"]
#[allow(
    dead_code,
    reason = "Only the separate workspace preview dispatches these operations."
)]
mod workspace_v2_native;

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
#[path = "native_settings.rs"]
mod settings;

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
#[path = "native_transfer_paths.rs"]
mod transfer_paths;

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
#[path = "native_deploy_inputs.rs"]
mod deploy_inputs;

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
#[path = "native_terminal_ui.rs"]
mod terminal_ui;

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
use terminal_ui::NativeMutationUi;

#[derive(Clone, Copy)]
#[cfg_attr(
    not(any(windows, feature = "headless", feature = "interactive")),
    allow(
        dead_code,
        reason = "The selected host is explicitly unsupported off Windows."
    )
)]
enum NativeCommand<'a> {
    #[cfg(any(windows, feature = "headless"))]
    Execution(
        &'a fabric_cli::execute::host::ApprovedExecution,
        std::time::Instant,
    ),
    Read(&'a ReadCommand),
    Job(&'a JobReadCommand),
    JobExecution(&'a JobRunCommand),
    JobMutation(&'a JobMutationCommand),
    Family(&'a Invocation),
}

fn has_bound_context(command: NativeCommand<'_>) -> bool {
    match command {
        NativeCommand::Family(Invocation::Tabular(export)) => matches!(
            export.command.as_ref(),
            Invocation::Read(_) | Invocation::JobRead(_)
        ),
        NativeCommand::Family(Invocation::Deploy(_)) => true,
        NativeCommand::Family(Invocation::Transfer(transfer)) => {
            transfer.kind != fabric_cli_transfer::TransferKind::Deploy
        }
        NativeCommand::Family(_) => false,
        _ => true,
    }
}

pub(crate) fn failure(code: &'static str, message: impl Into<String>, exit: u8) -> Failure {
    (code, message.into(), exit)
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn selected_read_options(command: &ReadCommand) -> Result<(), Failure> {
    if command.kind == fabric_cli::parser::ReadKind::Get {
        // Settings normalize presentation; the GET engine applies source preflight.
        return Ok(());
    }
    fabric_cli::session::get_options::preflight(command).map_err(|_| {
        failure(
            "NotSupported",
            "This GET option combination is not supported by the native host.",
            1,
        )
    })
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn consumes_definition_files(command: &Invocation) -> bool {
    matches!(command, Invocation::Deploy(_) | Invocation::Transfer(_))
}

#[cfg(any(windows, feature = "headless", feature = "interactive", test))]
fn interrupted_failure(code: &'static str, message: &str, exit: u8) -> Failure {
    failure(
        code,
        format!(
            "{message} Local waiting has stopped; in-flight remote or filesystem work may still complete. No rollback is implied."
        ),
        exit,
    )
}

#[cfg(any(windows, feature = "headless", feature = "interactive", test))]
fn finish_runtime<T>(runtime: tokio::runtime::Runtime, result: T) -> T {
    // Runtime Drop waits indefinitely for blocking syscalls; a bounded wait does
    // not cancel those calls or erase their possible filesystem effects.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn native_runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failure("runtime_failed", "The read runtime could not start.", 1))
}

#[cfg(any(windows, feature = "headless", feature = "interactive", test))]
async fn wait_for_deadline(deadline: Option<std::time::Duration>) {
    match deadline {
        Some(deadline) => tokio::time::sleep(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

fn write_error((code, message, exit): Failure) -> io::Result<CommandOutcome> {
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(
        &mut stderr,
        &json!({"error": {"code": code, "message": message}}),
    )
    .map_err(io::Error::other)?;
    writeln!(stderr)?;
    Ok(failure_outcome(code, exit))
}

fn failure_outcome(code: &str, exit: u8) -> CommandOutcome {
    match code {
        "cancelled" => CommandOutcome::Cancelled(exit),
        "signal_failed"
        | "timed_out"
        | "cleanup_pending"
        | "completion_unconfirmed"
        | "render_failed"
        | "output_failed"
        | "invalid_configuration"
        | "invalid_arguments"
        | "runtime_failed" => CommandOutcome::Stop(exit),
        _ => CommandOutcome::Returned(exit),
    }
}

#[cfg(not(all(windows, feature = "windows-broker")))]
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

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
enum NativeCredential {
    Broker {
        tenant: String,
        account: String,
        client: String,
    },
    #[cfg(feature = "headless")]
    Headless(crate::headless::Selection),
    #[cfg(feature = "interactive")]
    DeviceCode(crate::interactive_binding::Selection),
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
impl NativeCredential {
    fn tenant(&self) -> &str {
        match self {
            Self::Broker { tenant, .. } => tenant,
            #[cfg(feature = "interactive")]
            Self::DeviceCode(selection) => &selection.tenant,
            #[cfg(feature = "headless")]
            Self::Headless(selection) => &selection.tenant,
        }
    }

    fn account(&self) -> &str {
        match self {
            Self::Broker { account, .. } => account,
            #[cfg(feature = "interactive")]
            Self::DeviceCode(selection) => &selection.account,
            #[cfg(feature = "headless")]
            Self::Headless(selection) => &selection.account,
        }
    }

    fn is_broker(&self) -> bool {
        matches!(self, Self::Broker { .. })
    }

    fn metadata_endpoint(&self) -> Option<azure_core::http::Url> {
        match self {
            Self::Broker { .. } => None,
            #[cfg(feature = "interactive")]
            Self::DeviceCode(_) => None,
            #[cfg(feature = "headless")]
            Self::Headless(selection) => selection.metadata_endpoint(),
        }
    }

    fn bind(
        &self,
        state: &mut BatchState,
        home: fabric_cli_context::HomeContext,
    ) -> Result<(), fabric_cli_local::LocalError> {
        match self {
            Self::Broker {
                tenant,
                account,
                client,
            } => state.bind(tenant, account, client, home),
            #[cfg(feature = "interactive")]
            Self::DeviceCode(selection) => state.bind_provider(
                &selection.tenant,
                &selection.account,
                &selection.client,
                &selection.binding,
                home,
            ),
            #[cfg(feature = "headless")]
            Self::Headless(selection) => state.bind_provider(
                &selection.tenant,
                &selection.account,
                &selection.client,
                &selection.binding,
                home,
            ),
        }
    }

    fn bind_get(&self, state: &mut BatchState) -> Result<(), fabric_cli_local::LocalError> {
        match self {
            Self::Broker {
                tenant,
                account,
                client,
            } => state.bind_read_provider(tenant, account, client, "broker"),
            #[cfg(feature = "interactive")]
            Self::DeviceCode(selection) => state.bind_read_provider(
                &selection.tenant,
                &selection.account,
                &selection.client,
                &selection.binding,
            ),
            #[cfg(feature = "headless")]
            Self::Headless(selection) => state.bind_read_provider(
                &selection.tenant,
                &selection.account,
                &selection.client,
                &selection.binding,
            ),
        }
    }

    async fn connect(
        self,
        http: std::sync::Arc<dyn azure_core::http::HttpClient>,
    ) -> Result<std::sync::Arc<dyn azure_core::credentials::TokenCredential>, Failure> {
        match self {
            Self::Broker {
                tenant,
                account,
                client,
            } => {
                let _ = &http;
                #[cfg(all(windows, feature = "windows-broker"))]
                {
                    let options = fabric_identity::LoginOptions::new(&tenant, &client, &account)
                        .map_err(|_| {
                            failure("invalid_configuration", "Invalid broker selection.", 2)
                        })?;
                    fabric_identity::WindowsCredential::connect(options)
                        .await
                        .map(|credential| {
                            credential
                                as std::sync::Arc<dyn azure_core::credentials::TokenCredential>
                        })
                        .map_err(|error| {
                            let (code, message) = crate::auth::broker_error(error);
                            failure(code, message, 1)
                        })
                }
                #[cfg(not(all(windows, feature = "windows-broker")))]
                {
                    let _ = (tenant, account, client);
                    Err(failure(
                        "unsupported",
                        "The selected broker host is unavailable.",
                        1,
                    ))
                }
            }
            #[cfg(feature = "headless")]
            Self::Headless(selection) => selection.credential(http),
            #[cfg(feature = "interactive")]
            Self::DeviceCode(selection) => {
                fabric_identity::interactive::InteractiveCredential::connect(
                    selection.options,
                    http,
                    selection.store,
                )
                .await
                .map(|credential| {
                    credential as std::sync::Arc<dyn azure_core::credentials::TokenCredential>
                })
                .map_err(|error| {
                    let (code, message) = crate::interactive::failure(error);
                    failure(code, message, 1)
                })
            }
        }
    }
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn selected_credential() -> Result<NativeCredential, Failure> {
    let mode = match std::env::var("FAB_RUST_AUTH_MODE") {
        Ok(mode) if !mode.is_empty() => mode,
        Err(std::env::VarError::NotPresent) => {
            #[cfg(feature = "interactive")]
            if !cfg!(all(windows, feature = "windows-broker"))
                && crate::interactive_binding::store()
                    .and_then(|store| store.selection())
                    .map_err(|error| {
                        let (code, message) = crate::interactive::failure(error);
                        failure(code, message, 1)
                    })?
                    .is_some_and(|saved| saved.session_id.is_some())
            {
                return crate::interactive_binding::selected()
                    .map(NativeCredential::DeviceCode)
                    .map_err(|(code, message)| failure(code, message, 1));
            }
            if cfg!(feature = "native-read") {
                "broker".into()
            } else {
                return Err(failure(
                    "NotConfigured",
                    "An explicit FAB_RUST_AUTH_MODE is required.",
                    1,
                ));
            }
        }
        _ => {
            return Err(failure(
                "invalid_configuration",
                "Invalid native credential mode.",
                2,
            ));
        }
    };
    #[cfg(feature = "interactive")]
    if mode == "device-code" {
        return crate::interactive_binding::selected()
            .map(NativeCredential::DeviceCode)
            .map_err(|(code, message)| failure(code, message, 1));
    }
    if mode == "broker" {
        #[cfg(all(windows, feature = "windows-broker"))]
        {
            let options = crate::auth_binding::selected().map_err(|(code, message)| {
                failure(
                    code,
                    message,
                    if code == "invalid_configuration" {
                        2
                    } else {
                        1
                    },
                )
            })?;
            return Ok(NativeCredential::Broker {
                tenant: options.tenant().into(),
                account: options.account().into(),
                client: options.client().into(),
            });
        }
        #[cfg(not(all(windows, feature = "windows-broker")))]
        {
            let tenant = required("FAB_RUST_TENANT")?;
            let account = required("FAB_RUST_ACCOUNT")?;
            let client = required("FAB_RUST_CLIENT_ID")?;
            #[cfg(all(windows, feature = "windows-broker"))]
            fabric_identity::LoginOptions::new(&tenant, &client, &account)
                .map_err(|_| failure("invalid_configuration", "Invalid broker selection.", 2))?;
            if !cfg!(all(windows, feature = "windows-broker")) {
                return Err(failure(
                    "unsupported",
                    "The selected broker host is unavailable.",
                    1,
                ));
            }
            return Ok(NativeCredential::Broker {
                tenant,
                account,
                client,
            });
        }
    }
    #[cfg(feature = "headless")]
    return crate::headless::Selection::capture(&mode).map(NativeCredential::Headless);
    #[cfg(not(feature = "headless"))]
    Err(failure(
        "invalid_configuration",
        "The selected credential mode is unavailable.",
        2,
    ))
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

pub(crate) fn run(command: &ReadCommand, state: &mut BatchState) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::Read(command), state)
}

pub(crate) fn run_execute(
    command: &fabric_cli::execute::ExecuteCommand,
    state: &mut BatchState,
) -> io::Result<CommandOutcome> {
    #[cfg(any(windows, feature = "headless"))]
    return execute::run(command, state);
    #[cfg(not(any(windows, feature = "headless")))]
    {
        let _ = (command, state);
        write_error(failure(
            "unsupported",
            "The native execution host is unavailable.",
            1,
        ))
    }
}

pub(crate) fn run_job(
    command: &JobReadCommand,
    state: &mut BatchState,
) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::Job(command), state)
}

pub(crate) fn run_job_execution(
    command: &JobRunCommand,
    state: &mut BatchState,
) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::JobExecution(command), state)
}

pub(crate) fn run_job_mutation(
    command: &JobMutationCommand,
    state: &mut BatchState,
) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::JobMutation(command), state)
}

#[cfg(feature = "v2-cancel")]
pub(crate) fn run_cancel(
    command: &fabric_cli::parser::CancelCommand,
    state: Option<&mut BatchState>,
) -> io::Result<CommandOutcome> {
    use fabric_cli::cancel::{Dispatch, preflight};
    let target = match preflight(command) {
        Ok(target) => target,
        Err(error) => {
            let delivery = Dispatch::unavailable(None, None, error.code(), &error.to_string(), 2);
            return Ok(CommandOutcome::Stop(
                delivery.write_to(&mut io::stdout().lock(), &mut io::stderr().lock()),
            ));
        }
    };
    let endpoint = match selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC")) {
        Ok(endpoint) => endpoint,
        Err((code, _, exit)) => {
            let delivery = Dispatch::unavailable(
                Some(&target),
                None,
                code,
                "Invalid cancellation host configuration.",
                exit,
            );
            return Ok(CommandOutcome::Stop(
                delivery.write_to(&mut io::stdout().lock(), &mut io::stderr().lock()),
            ));
        }
    };
    #[cfg(any(windows, feature = "headless", feature = "interactive"))]
    let result = selected_credential()
        .and_then(|credential| run_cancel_selected(command, credential, endpoint.clone(), state));
    #[cfg(not(any(windows, feature = "headless", feature = "interactive")))]
    let result: Result<Dispatch, Failure> = (|| {
        let _ = state;
        required("FAB_RUST_TENANT")?;
        required("FAB_RUST_ACCOUNT")?;
        required("FAB_RUST_CLIENT_ID")?;
        Err(failure(
            "unsupported",
            "Native credentials currently require Windows.",
            1,
        ))
    })();
    let delivery = match result {
        Ok(delivery) => delivery,
        Err((code, _, exit)) => Dispatch::unavailable(
            Some(&target),
            Some(&endpoint),
            code,
            "Cancellation host is unavailable.",
            exit,
        ),
    };
    let exit = delivery.write_to(&mut io::stdout().lock(), &mut io::stderr().lock());
    Ok(if exit != delivery.exit_code() || exit == 2 {
        CommandOutcome::Stop(exit)
    } else if delivery.value()["error"]["interruption"] == "cancelled" {
        CommandOutcome::Cancelled(exit)
    } else if delivery.value()["error"]["interruption"].is_string() {
        CommandOutcome::Stop(exit)
    } else {
        CommandOutcome::Returned(exit)
    })
}

#[cfg(all(
    feature = "v2-cancel",
    any(windows, feature = "headless", feature = "interactive")
))]
fn run_cancel_selected(
    command: &fabric_cli::parser::CancelCommand,
    credential: NativeCredential,
    endpoint: EndpointBinding,
    state: Option<&mut BatchState>,
) -> Result<fabric_cli::cancel::Dispatch, Failure> {
    use std::sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    };
    // Explicit IDs need no navigation, metadata snapshot or saved output format.
    if let Some(state) = state {
        credential
            .bind_get(state)
            .map_err(|_| failure("invalid_configuration", "Invalid credential binding.", 2))?;
    }
    let (client, identity_client) = native_clients(&credential, false)?;
    let runtime = native_runtime()?;
    let reason = Arc::new(AtomicU8::new(0));
    let signal_reason = reason.clone();
    let deadline = fabric_sdk_http::ExecutionOptions::default().timeout;
    let result = runtime.block_on(execute_cancel_injected(
        command,
        (credential, client, identity_client),
        endpoint,
        async move {
            tokio::select! {
                biased;
                signal = tokio::signal::ctrl_c() => {
                    signal_reason.store(if signal.is_ok() { 1 } else { 2 }, Ordering::Relaxed);
                }
                _ = wait_for_deadline(Some(deadline)) => {
                    signal_reason.store(3, Ordering::Relaxed);
                }
            }
        },
    ));
    let result = match reason.load(Ordering::Relaxed) {
        1 => result.interrupted("cancelled"),
        2 => result.interrupted("signal_failed"),
        3 => result.interrupted("timed_out"),
        _ => result,
    };
    Ok(finish_runtime(runtime, result))
}

#[cfg(all(
    feature = "v2-cancel",
    any(windows, feature = "headless", feature = "interactive")
))]
async fn execute_cancel_injected(
    command: &fabric_cli::parser::CancelCommand,
    (credential, client, identity_client): (
        NativeCredential,
        std::sync::Arc<dyn azure_core::http::HttpClient>,
        std::sync::Arc<dyn azure_core::http::HttpClient>,
    ),
    endpoint: EndpointBinding,
    cancelled: impl std::future::Future<Output = ()> + Send,
) -> fabric_cli::cancel::Dispatch {
    use fabric_cli::cancel::{Dispatch, preflight};
    use fabric_sdk::{Audience, CredentialSelection, RequestContext};
    use futures_util::future::{Either, select};
    use std::sync::Arc;

    let target = match preflight(command) {
        Ok(target) => target,
        Err(error) => {
            return Dispatch::unavailable(None, None, error.code(), &error.to_string(), 2);
        }
    };
    let unavailable = |code: &str, exit| {
        Dispatch::unavailable(
            Some(&target),
            Some(&endpoint),
            code,
            "Cancellation host is unavailable.",
            exit,
        )
    };
    let selection =
        match CredentialSelection::new(credential.account(), credential.tenant(), Audience::Fabric)
        {
            Ok(selection) => selection,
            Err(_) => return unavailable("invalid_configuration", 2),
        };
    let (credential, cancelled) = match select(
        Box::pin(credential.connect(identity_client)),
        Box::pin(cancelled),
    )
    .await
    {
        Either::Left((Ok(credential), cancelled)) => (credential, cancelled),
        Either::Left((Err((code, _, exit)), _)) => return unavailable(code, exit),
        Either::Right(_) => return unavailable("Cancelled", 1),
    };
    let executor = match native_executor(
        client,
        endpoint.clone(),
        fabric_sdk_http::ExecutionOptions::default(),
    ) {
        Ok(executor) => executor,
        Err((code, _, exit)) => return unavailable(code, exit),
    };
    let sdk = fabric_sdk_jobs::JobClient::with_context(
        RequestContext::with_endpoint(credential, selection, endpoint),
        Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor)),
        fabric_sdk_jobs::JobLimits::default(),
    );
    fabric_cli::cancel::dispatch(command, &sdk, cancelled).await
}

pub(crate) fn run_family(
    command: &Invocation,
    state: &mut BatchState,
) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::Family(command), state)
}

fn run_command(command: NativeCommand<'_>, state: &mut BatchState) -> io::Result<CommandOutcome> {
    let result = (|| {
        let endpoint = selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
        if endpoint != EndpointBinding::public(ServiceOrigin::Fabric) && !has_bound_context(command)
        {
            return Err(failure(
                "NotSupported",
                "The selected endpoint is not yet wired for this command family.",
                1,
            ));
        }
        #[cfg(any(windows, feature = "headless", feature = "interactive"))]
        {
            let credential = selected_credential()?;
            run_selected(command, credential, endpoint, state)
        }
        #[cfg(not(any(windows, feature = "headless", feature = "interactive")))]
        {
            let tenant = required("FAB_RUST_TENANT")?;
            let account = required("FAB_RUST_ACCOUNT")?;
            let client = required("FAB_RUST_CLIENT_ID")?;
            run_selected(command, &tenant, &account, &client, endpoint, state)
        }
    })();
    match result {
        Ok(delivery) => delivery.write_to(&mut io::stdout().lock(), &mut io::stderr().lock()),
        Err((code, message, exit))
            if matches!(command, NativeCommand::Family(Invocation::Tabular(_))) =>
        {
            let NativeCommand::Family(Invocation::Tabular(export)) = command else {
                unreachable!("matched explicit export");
            };
            let source =
                fabric_cli::tabular_export::source(&export.command).unwrap_or("unsupported");
            let delivery = fabric_cli::tabular_export::ExportDelivery::host_failed(
                source,
                &export.request,
                code,
                &message,
                exit,
            );
            delivery
                .write_to_streams(&mut io::stdout().lock(), &mut io::stderr().lock())
                .map_err(fabric_cli::tabular_export::ReceiptWriteFailure::into_io_error)?;
            Ok(failure_outcome(code, exit))
        }
        Err(error) => write_error(error),
    }
}

#[cfg(not(any(windows, feature = "headless", feature = "interactive")))]
fn run_selected(
    _: NativeCommand<'_>,
    _: &str,
    _: &str,
    _: &str,
    _: EndpointBinding,
    _: &mut BatchState,
) -> Result<get::Delivery, Failure> {
    Err(failure(
        "unsupported",
        "Native read credentials currently require Windows.",
        1,
    ))
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn run_selected(
    command: NativeCommand<'_>,
    credential: NativeCredential,
    endpoint: EndpointBinding,
    state: &mut BatchState,
) -> Result<get::Delivery, Failure> {
    let current = state
        .path()
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    let home = match state
        .home()
        .map_err(|message| failure("invalid_configuration", message, 2))?
    {
        Some(home) => selected_home(home)?,
        None => fabric_cli_context::HomeContext::Unspecified,
    };
    let is_get = matches!(
        command,
        NativeCommand::Read(read) if read.kind == fabric_cli::parser::ReadKind::Get
    );
    if is_get {
        credential.bind_get(state)
    } else {
        credential.bind(state, home.clone())
    }
    .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    if let NativeCommand::Read(command) = command {
        selected_read_options(command)?;
    }
    let skill = match command {
        #[cfg(any(windows, feature = "headless"))]
        NativeCommand::Execution(..) => None,
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
    let saved = crate::local_host::available_store()
        .and_then(|store| store.map(|store| store.initialize()).transpose())
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    let settings = settings::NativeSettings::select(saved.as_ref())
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    let job = match command {
        NativeCommand::JobExecution(command) => Some(command.family_command()),
        NativeCommand::JobMutation(command) => Some(command.family_command()),
        _ => None,
    };
    let deadline = settings::command_watchdog(job.as_ref(), settings.jobs)
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    if deadline.is_some_and(|deadline| std::time::Instant::now().checked_add(deadline).is_none()) {
        return Err(failure(
            "invalid_configuration",
            "The selected job timeout exceeds the native deadline range.",
            2,
        ));
    }
    let runtime = native_runtime()?;
    let before = state
        .navigation
        .as_ref()
        .map(|navigation| std::sync::Arc::clone(navigation.current()));
    let result = runtime.block_on(async {
        if is_get {
            let clock = get::NativeClock(tokio::runtime::Handle::current());
            let resource_config = settings.get_resources.clone();
            get::controlled(
                async |evidence, cancelled| {
                    execute_delivery(
                        command, credential, &current, home, endpoint, settings, state,
                        Some(get::Inputs {
                            evidence, cancelled, clock: &clock, ui: &NativeTransferUi,
                            files: get::Files::native(),
                            resource_config,
                        }),
                    ).await
                },
                async {
                    tokio::select! {
                        biased;
                        signal = tokio::signal::ctrl_c() => match signal {
                            Ok(()) => interrupted_failure("cancelled", "Read cancelled.", 2),
                            Err(_) => failure("signal_failed", "The cancellation handler could not start.", 1),
                        },
                        _ = wait_for_deadline(deadline) => interrupted_failure(
                            "timed_out", "The read command deadline elapsed.", 1,
                        ),
                    }
                },
            ).await
        } else {
            tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                match signal {
                    Ok(()) => Err(interrupted_failure("cancelled", "Read cancelled.", 2)),
                    Err(_) => Err(failure("signal_failed", "The cancellation handler could not start.", 1)),
                }
            },
            _ = wait_for_deadline(deadline) => {
                Err(interrupted_failure("timed_out", "The read command deadline elapsed.", 1))
            },
            result = execute_delivery(command, credential, &current, home, endpoint, settings, state, None) => result,
            }
        }
    });
    if let Some(before) = before {
        state.commit_navigation(&before);
    }
    finish_runtime(runtime, result)
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
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

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
#[allow(
    clippy::too_many_arguments,
    reason = "Borrowed command capabilities, not a second runtime."
)]
async fn execute_delivery(
    command: NativeCommand<'_>,
    credential: NativeCredential,
    current: &str,
    home: fabric_cli_context::HomeContext,
    endpoint: EndpointBinding,
    settings: settings::NativeSettings,
    state: &mut BatchState,
    get_inputs: Option<get::Inputs<'_>>,
) -> Result<get::Delivery, Failure> {
    #[cfg(any(windows, feature = "headless"))]
    let execution = matches!(command, NativeCommand::Execution(..));
    #[cfg(not(any(windows, feature = "headless")))]
    let execution = false;
    let (client, identity_client) = native_clients(&credential, execution)?;
    execute_injected_delivery(
        command,
        (credential, client, identity_client),
        current,
        home,
        endpoint,
        settings,
        state,
        get_inputs,
    )
    .await
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
type NativeClients = (
    std::sync::Arc<dyn azure_core::http::HttpClient>,
    std::sync::Arc<dyn azure_core::http::HttpClient>,
);

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn native_clients(
    credential: &NativeCredential,
    execution: bool,
) -> Result<NativeClients, Failure> {
    native_clients_with_options(credential, native_transport_options(execution))
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn native_clients_with_options(
    credential: &NativeCredential,
    options: fabric_sdk_transport::TransportOptions,
) -> Result<NativeClients, Failure> {
    use fabric_sdk_transport::NativeHttpClient;
    use std::sync::Arc;

    let identity_client = credential
        .metadata_endpoint()
        .map(|endpoint| {
            NativeHttpClient::for_managed_identity(options.clone(), endpoint)
                .map(|client| Arc::new(client) as Arc<dyn azure_core::http::HttpClient>)
        })
        .transpose()
        .map_err(|_| {
            failure(
                "invalid_configuration",
                "Invalid managed identity transport configuration.",
                2,
            )
        })?;
    let client: Arc<dyn azure_core::http::HttpClient> =
        Arc::new(NativeHttpClient::new(options).map_err(|_| {
            failure(
                "transport_failed",
                "The native HTTP client could not start.",
                1,
            )
        })?);
    Ok((client.clone(), identity_client.unwrap_or(client)))
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn native_transport_options(execution: bool) -> fabric_sdk_transport::TransportOptions {
    use fabric_sdk_transport::{ProxyPolicy, TransportOptions, TrustPolicy};
    use std::{num::NonZeroUsize, time::Duration};

    TransportOptions {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(if execution {
            // Execute Query has a service-side 90-second evaluation window.
            95
        } else {
            30
        }),
        write_timeout: Duration::from_secs(30),
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(4).expect("nonzero constant"),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    }
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn native_executor(
    client: std::sync::Arc<dyn azure_core::http::HttpClient>,
    endpoint: EndpointBinding,
    options: fabric_sdk_http::ExecutionOptions,
) -> Result<fabric_sdk_http::HttpExecutor, Failure> {
    fabric_sdk_http::HttpExecutor::new(
        client,
        std::sync::Arc::new(fabric_sdk_transport::TokioTimer),
        options,
    )
    .map(|executor| executor.with_endpoint(endpoint))
    .map_err(|_| failure("transport_failed", "The HTTP executor could not start.", 1))
}

#[cfg(all(test, feature = "headless"))]
async fn execute_injected(
    command: NativeCommand<'_>,
    (credential, client, identity_client): (
        NativeCredential,
        std::sync::Arc<dyn azure_core::http::HttpClient>,
        std::sync::Arc<dyn azure_core::http::HttpClient>,
    ),
    current: &str,
    home: fabric_cli_context::HomeContext,
    endpoint: EndpointBinding,
    settings: settings::NativeSettings,
    state: &mut BatchState,
) -> Result<(fabric_cli_output::text::TextPlan, CommandOutcome), Failure> {
    execute_injected_delivery(
        command,
        (credential, client, identity_client),
        current,
        home,
        endpoint,
        settings,
        state,
        None,
    )
    .await
    .map(get::Delivery::into_plan_parts)
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
#[allow(
    clippy::too_many_arguments,
    reason = "The existing native composition plus explicit GET capabilities."
)]
async fn execute_injected_delivery(
    command: NativeCommand<'_>,
    (credential, client, identity_client): (
        NativeCredential,
        std::sync::Arc<dyn azure_core::http::HttpClient>,
        std::sync::Arc<dyn azure_core::http::HttpClient>,
    ),
    current: &str,
    home: fabric_cli_context::HomeContext,
    endpoint: EndpointBinding,
    settings: settings::NativeSettings,
    state: &mut BatchState,
    get_inputs: Option<get::Inputs<'_>>,
) -> Result<get::Delivery, Failure> {
    let broker_identity = credential.is_broker();
    let deadline = match command {
        #[cfg(any(windows, feature = "headless"))]
        NativeCommand::Execution(_, deadline) => Some(deadline),
        _ => None,
    };
    let (sdk, executor) = connect_native(
        credential,
        client,
        identity_client,
        endpoint,
        fabric_sdk_http::ExecutionOptions::default(),
        deadline,
    )
    .await?;
    execute_authenticated_injected(
        command,
        sdk,
        executor,
        current,
        home,
        settings,
        state,
        broker_identity,
        get_inputs,
    )
    .await
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
async fn connect_native(
    credential: NativeCredential,
    client: std::sync::Arc<dyn azure_core::http::HttpClient>,
    identity_client: std::sync::Arc<dyn azure_core::http::HttpClient>,
    endpoint: EndpointBinding,
    options: fabric_sdk_http::ExecutionOptions,
    deadline: Option<std::time::Instant>,
) -> Result<(fabric_sdk::Client, fabric_sdk_http::HttpExecutor), Failure> {
    use fabric_sdk::{Audience, Client, CredentialSelection, RequestContext};

    let selection =
        CredentialSelection::new(credential.account(), credential.tenant(), Audience::Fabric)
            .map_err(|_| failure("invalid_configuration", "Invalid credential selection.", 2))?;
    let credential = match deadline {
        #[cfg(any(windows, feature = "headless"))]
        Some(deadline) => {
            execute::before_submission(credential.connect(identity_client), deadline).await?
        }
        #[cfg(not(any(windows, feature = "headless")))]
        Some(_) => {
            return Err(failure(
                "unsupported",
                "The native execution host is unavailable.",
                1,
            ));
        }
        None => credential.connect(identity_client).await?,
    };
    let sdk = Client::with_context(RequestContext::with_endpoint(
        credential,
        selection,
        endpoint.clone(),
    ));
    let executor = native_executor(client, endpoint, options)?;
    Ok((sdk, executor))
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
#[allow(
    clippy::too_many_arguments,
    reason = "The existing authenticated host with explicit GET capabilities."
)]
async fn execute_authenticated_injected(
    command: NativeCommand<'_>,
    sdk: fabric_sdk::Client,
    executor: fabric_sdk_http::HttpExecutor,
    current: &str,
    home: fabric_cli_context::HomeContext,
    settings: settings::NativeSettings,
    state: &mut BatchState,
    broker_identity: bool,
    get_inputs: Option<get::Inputs<'_>>,
) -> Result<get::Delivery, Failure> {
    #[cfg(any(windows, feature = "headless"))]
    if let NativeCommand::Execution(approved, deadline) = command {
        return execute::dispatch(approved, deadline, &sdk, executor).await;
    }
    use fabric_cli::session::ReadSession;
    use std::sync::Arc;
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
        tenant: fabric_cli_context::Element::tenant(
            sdk.selection().tenant(),
            sdk.selection().tenant(),
        ),
        current_path: current,
        home,
        timestamp,
        config: settings.reads,
    };
    let result = match command {
        #[cfg(any(windows, feature = "headless"))]
        NativeCommand::Execution(..) => unreachable!("execution uses its typed result host"),
        NativeCommand::Read(command) => {
            let command = settings
                .read_command(command)
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            if command.kind == fabric_cli::parser::ReadKind::Get {
                Ok(get::dispatch(&session, &executor, &command, get_inputs).await)
            } else {
                session
                    .with_source_operations(&executor)
                    .dispatch_with_evidence(&command, std::future::pending())
                    .await
                    .map(|output| {
                        let outcome = crate::batch_state::read_outcome(&output);
                        get::Delivery::plan(output.plan, outcome)
                    })
            }
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
            .dispatch_with_evidence(&command, std::future::pending())
            .await
            .map(job_dispatch)
            .map(|(plan, outcome)| get::Delivery::plan(plan, outcome))
        }
        NativeCommand::JobExecution(command) => {
            let command = settings
                .job_command(&command.family_command())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            execute_job(&session, &executor, &command, settings.jobs)
                .await
                .map(job_dispatch)
                .map(|(plan, outcome)| get::Delivery::plan(plan, outcome))
        }
        NativeCommand::JobMutation(command) => {
            let command = settings
                .job_command(&command.family_command())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            execute_job(&session, &executor, &command, settings.jobs)
                .await
                .map(job_dispatch)
                .map(|(plan, outcome)| get::Delivery::plan(plan, outcome))
        }
        NativeCommand::Family(command) => {
            execute_family(&session, &executor, command, state, broker_identity).await
        }
    };
    match result {
        Ok(mut delivery) => {
            if state
                .take_restored()
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?
            {
                use fabric_cli_output::text::{self, Presentation};
                let message = format!(
                    "Command context path: {}",
                    if current == "/" { "root" } else { current }
                );
                let warning = text::render_notice(
                    text::Notice::Warning(&message),
                    None,
                    Presentation::default(),
                );
                delivery.prepend(warning);
            }
            Ok(delivery)
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

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn job_dispatch(
    output: fabric_cli::jobs::JobDispatch,
) -> (fabric_cli_output::text::TextPlan, CommandOutcome) {
    let outcome = crate::batch_state::job_outcome(&output);
    (output.plan, outcome)
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
async fn execute_job(
    session: &fabric_cli::session::ReadSession<'_>,
    executor: &fabric_sdk_http::HttpExecutor,
    command: &fabric_cli_jobs::JobCommand,
    config: fabric_cli_jobs::JobConfig,
) -> Result<fabric_cli::jobs::JobDispatch, fabric_cli_output::text::TextError> {
    fabric_cli::jobs::JobSession {
        reads: session,
        jobs: std::sync::Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
        files: &fabric_cli::jobs::SelectedJobFiles,
        ui: &NativeJobUi,
        config,
    }
    .dispatch_with_evidence(command, std::future::pending())
    .await
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
pub(crate) async fn execute_tabular_family(
    export: &fabric_cli::tabular_export::ExportCommand,
    families: &mut fabric_cli::families::FamilySession<'_, '_>,
    sink: &mut impl fabric_cli::tabular_export::ExportSink,
) -> (fabric_cli::tabular_export::ExportDelivery, CommandOutcome) {
    use fabric_cli::tabular_export::{self, ExecutionFailure};
    use fabric_cli_output::text::OutputFormat;

    let session = families.reads;
    let executor = families.http;
    let mut original = None;
    let captured = &mut original;
    let delivery = tabular_export::dispatch(export, sink, |_| async move {
        let result = match export.command.as_ref() {
            Invocation::Read(read) => tabular_export::read_rows(session, read).await,
            Invocation::JobRead(job) => {
                tabular_export::job_rows(
                    &fabric_cli::jobs::JobReadSession {
                        reads: session,
                        jobs: std::sync::Arc::new(fabric_cli_jobs::HttpJobExecutor::new(
                            executor.clone(),
                        )),
                    },
                    job,
                )
                .await
            }
            invocation => {
                let mut invocation = invocation.clone();
                match &mut invocation {
                    Invocation::Access(access) => {
                        access.output_format = Some(OutputFormat::Json);
                    }
                    Invocation::Table(table) => table.output_format = Some("json".into()),
                    _ => {}
                }
                families
                    .dispatch_tabular(&invocation, std::future::pending())
                    .await
                    .map_err(|_| {
                        *captured = Some(CommandOutcome::Stop(1));
                        ExecutionFailure {
                            code: "export_adapter_failed".into(),
                            message: "Native result presentation failed; execution is not retried"
                                .into(),
                        }
                    })
                    .and_then(|output| {
                        *captured = Some(crate::batch_state::family_outcome(&output));
                        tabular_export::family_rows(&invocation, output)
                    })
            }
        };
        if captured.is_none()
            && let Err(error) = &result
        {
            let error = fabric_cli_output::CommandError::new(
                Some(error.message.clone()),
                Some(error.code.clone()),
            );
            match crate::batch_state::error_outcome(&error, 1) {
                CommandOutcome::Returned(_) => {}
                outcome => *captured = Some(outcome),
            }
        }
        result
    })
    .await;
    retain_export_outcome(delivery, original)
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
pub(crate) fn retain_export_outcome(
    delivery: fabric_cli::tabular_export::ExportDelivery,
    original: Option<CommandOutcome>,
) -> (fabric_cli::tabular_export::ExportDelivery, CommandOutcome) {
    // Unrendered family plans use a zero placeholder; the receipt owns the real exit.
    let outcome = match original {
        Some(CommandOutcome::Stop(_)) => CommandOutcome::Stop(delivery.exit_code),
        Some(CommandOutcome::Cancelled(_)) => CommandOutcome::Cancelled(delivery.exit_code),
        _ => CommandOutcome::Returned(delivery.exit_code),
    };
    (delivery, outcome)
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
async fn execute_family(
    session: &fabric_cli::session::ReadSession<'_>,
    executor: &fabric_sdk_http::HttpExecutor,
    command: &Invocation,
    state: &mut BatchState,
    broker_identity: bool,
) -> Result<get::Delivery, fabric_cli_output::text::TextError> {
    use fabric_cli_output::text::{self, OutputFormat, Presentation, TextOptions};

    let result = state
        .family(async |navigation| {
            let store = crate::local_host::store()?;
            store.initialize()?;
            let local_root = std::env::current_dir()?;
            let physical_files =
                fabric_cli::families::native_resource_files::NativeResourceFileInputs::new(
                    local_root.clone(),
                    tokio::runtime::Handle::current(),
                );
            let mut effects = NativeLocalEffects {
                base: crate::local_host::Effects,
                identity: fabric_cli_local::BrowserIdentity {
                    user: broker_identity,
                    claims: if broker_identity {
                        vec![
                            ("upn".into(), session.sdk.selection().account().into()),
                            ("tid".into(), session.sdk.selection().tenant().into()),
                        ]
                    } else {
                        Vec::new()
                    },
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
            let mut mutations = NativeMutationUi::default();
            let mut confirmation = terminal_ui::NativeAccessConfirmation::default();
            let mut files = if consumes_definition_files(command) {
                let files: Box<dyn fabric_cli_transfer::TransferFiles> =
                    if let Invocation::Deploy(deploy) = command {
                        let argument = deploy.config.clone().ok_or_else(|| {
                            fabric_cli_local::LocalError::command(
                                "InvalidInput",
                                "A deployment configuration path is required",
                            )
                        })?;
                        let home = transfer_paths::home_from_environment();
                        let candidate = transfer_paths::candidate(
                            &argument,
                            &local_root,
                            home.as_deref(),
                            std::env::var_os("USERNAME").as_deref(),
                        );
                        Box::new(
                            fabric_cli::families::transfer_files::TokioDeployFiles::defer_config(
                                candidate,
                                argument,
                                Default::default(),
                                tokio::runtime::Handle::current(),
                            ),
                        )
                    } else if let Invocation::Transfer(transfer) = command
                        && matches!(
                            transfer.kind,
                            fabric_cli_transfer::TransferKind::Import
                                | fabric_cli_transfer::TransferKind::Export
                                | fabric_cli_transfer::TransferKind::BulkExport
                        )
                    {
                        let tokens = if transfer.kind == fabric_cli_transfer::TransferKind::Import {
                            transfer.input.as_deref()
                        } else {
                            transfer.output.as_deref()
                        };
                        let input =
                            tokens
                                .map(fabric_cli_transfer::process_tokens)
                                .ok_or_else(|| {
                                    fabric_cli_local::LocalError::command(
                                        "InvalidInput",
                                        "A transfer path is required",
                                    )
                                })?;
                        let home = transfer_paths::home_from_environment();
                        let candidate = transfer_paths::candidate(
                            &input,
                            &local_root,
                            home.as_deref(),
                            std::env::var_os("USERNAME").as_deref(),
                        );
                        if transfer.kind == fabric_cli_transfer::TransferKind::Import {
                            Box::new(
                        fabric_cli::families::transfer_files::TokioTransferFiles::defer_import_root(
                            candidate,
                            input,
                            Default::default(),
                            tokio::runtime::Handle::current(),
                        ),
                    )
                        } else {
                            Box::new(
                        fabric_cli::families::transfer_files::TokioTransferFiles::defer_export_root(
                            candidate,
                            input,
                            Default::default(),
                            tokio::runtime::Handle::current(),
                        ),
                    )
                        }
                    } else {
                        Box::new(
                            fabric_cli::families::transfer_files::TokioTransferFiles::new(
                                local_root,
                                None,
                                Default::default(),
                                tokio::runtime::Handle::current(),
                            )
                            .await?,
                        )
                    };
                Some(files)
            } else {
                None
            };
            let mut families = fabric_cli::families::FamilySession {
                reads: session,
                http: executor,
                store: &store,
                navigation,
                local: &mut effects,
                mutations: &mut mutations,
                confirmation: Some(&mut confirmation),
                transfer_ui: &NativeTransferUi,
                transfer_files: files
                    .as_mut()
                    .map(|files| files.as_mut() as &mut dyn fabric_cli_transfer::TransferFiles),
                resource_files: None,
            };
            if let Invocation::Tabular(export) = command {
                let (delivery, outcome) = execute_tabular_family(
                    export,
                    &mut families,
                    &mut fabric_cli::tabular_export::FileSink::default(),
                )
                .await;
                return Ok(get::Delivery::export(delivery, outcome));
            }
            let mut output = if matches!(command, Invocation::Deploy(_)) {
                let inputs = deploy_inputs::snapshot_environment().map_err(|error| {
                    fabric_cli_local::LocalError::command("InvalidInput", error.to_string())
                })?;
                families
                    .dispatch_with_deploy_inputs(command, &inputs, std::future::pending())
                    .await?
            } else if matches!(
                command,
                Invocation::Resource(
                    fabric_cli_mutations::frontdoor::ResourceCommand::Remove { .. }
                )
            ) {
                let mut removal = NativeMutationUi::default();
                let mut output = families
                    .dispatch_with_removal_ui(command, &mut removal, std::future::pending())
                    .await?;
                let mut notices = std::mem::take(&mut removal.records);
                notices.append(&mut output.plan.records);
                output.plan.records = notices;
                output
            } else if matches!(
                command,
                Invocation::Resource(
                    fabric_cli_mutations::frontdoor::ResourceCommand::Copy { .. }
                        | fabric_cli_mutations::frontdoor::ResourceCommand::Move { .. }
                )
            ) {
                families
                    .dispatch_with_physical_files(command, &physical_files, std::future::pending())
                    .await?
            } else {
                families.dispatch(command, std::future::pending()).await?
            };
            let mut notices = std::mem::take(&mut mutations.records);
            notices.append(&mut output.plan.records);
            output.plan.records = notices;
            let outcome = crate::batch_state::family_outcome(&output);
            Ok::<_, fabric_cli_local::LocalError>(get::Delivery::plan(output.plan, outcome))
        })
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
        )
        .map(|plan| {
            let outcome = crate::batch_state::error_outcome(&error.command_error(), plan.exit_code);
            get::Delivery::plan(plan, outcome)
        }),
    }
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
struct NativeLocalEffects {
    base: crate::local_host::Effects,
    identity: fabric_cli_local::BrowserIdentity,
    capacities: Option<Vec<fabric_cli_local::Capacity>>,
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
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

#[cfg_attr(
    not(any(windows, feature = "headless", feature = "interactive")),
    allow(dead_code)
)]
struct NativeTransferUi;

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
        question: &'a str,
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<bool, fabric_cli_output::CommandError>>
    {
        Box::pin(async move { fabric_cli::console::confirm(question).await })
    }
    fn select<'a>(
        &'a self,
        names: &'a [String],
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<Vec<String>, fabric_cli_output::CommandError>>
    {
        Box::pin(async move { fabric_cli::console::select_items(names).await })
    }
}
#[cfg(any(windows, feature = "headless", feature = "interactive"))]
struct NativeJobUi;

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
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
        Box::pin(async { fabric_cli::console::confirm("Are you sure?").await })
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

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[cfg(any(windows, feature = "headless"))]
    #[test]
    fn dataflow_execution_alone_selects_the_extended_transport_read_window() {
        assert_eq!(
            native_transport_options(true).read_timeout,
            std::time::Duration::from_secs(95)
        );
        assert_eq!(
            native_transport_options(false).read_timeout,
            std::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn cancellation_and_configuration_failure_do_not_share_a_control_reason() {
        assert_eq!(
            failure_outcome("cancelled", 2),
            CommandOutcome::Cancelled(2)
        );
        assert_eq!(
            failure_outcome("invalid_configuration", 2),
            CommandOutcome::Stop(2)
        );
        assert_eq!(failure_outcome("timed_out", 1), CommandOutcome::Stop(1));
    }

    #[cfg(windows)]
    #[test]
    fn physical_copy_does_not_activate_the_definition_filesystem_boundary() {
        for verb in ["cp", "mv"] {
            let command =
                fabric_cli::parser::parse_args(["fab-rs", verb, "source.bin", "target.bin"])
                    .unwrap();
            assert!(!consumes_definition_files(&command));
        }
        let command = fabric_cli::parser::parse_args([
            "fab-rs",
            "export",
            "Demo.Workspace/Book.Notebook",
            "-o",
            "output",
            "-f",
        ])
        .unwrap();
        assert!(consumes_definition_files(&command));
    }

    #[cfg(windows)]
    #[test]
    fn native_get_option_gate_defers_get_output_to_full_engine() {
        for flag in ["--force", "--verbose"] {
            let Invocation::Read(command) = fabric_cli::parser::parse_args([
                "fab-rs",
                "get",
                "Demo.Workspace/Book.Notebook",
                "-q",
                "id",
                flag,
            ])
            .unwrap() else {
                panic!("expected GET invocation");
            };
            selected_read_options(&command).unwrap();
        }
        let Invocation::Read(command) = fabric_cli::parser::parse_args([
            "fab-rs",
            "get",
            "Demo.Workspace",
            "--output",
            "private-output-path",
        ])
        .unwrap() else {
            panic!("expected GET invocation");
        };
        selected_read_options(&command).unwrap();
        assert_eq!(command.output.as_deref(), Some("private-output-path"));
    }

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
        future::{Future, poll_fn},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    #[test]
    fn no_watchdog_stays_pending_and_a_finite_watchdog_elapses() {
        runtime().block_on(async {
            let mut unbounded = std::pin::pin!(wait_for_deadline(None));
            poll_fn(|context| {
                assert!(unbounded.as_mut().poll(context).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_deadline(Some(Duration::ZERO)),
            )
            .await
            .expect("an explicit finite watchdog must still elapse");
        });
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
