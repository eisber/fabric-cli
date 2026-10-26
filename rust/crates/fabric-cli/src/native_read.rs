// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit opt-in binary composition. Libraries never read these host inputs.

use fabric_cli::{
    HostError,
    parser::{JobReadCommand, ReadCommand},
};
use serde_json::json;
use std::{
    io::{self, Write},
    process::ExitCode,
};

type Failure = (&'static str, String, u8);

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
}

fn failure(code: &'static str, message: impl Into<String>, exit: u8) -> Failure {
    (code, message.into(), exit)
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

pub(crate) fn run(command: &ReadCommand) -> io::Result<ExitCode> {
    run_command(NativeCommand::Read(command))
}

pub(crate) fn run_job(command: &JobReadCommand) -> io::Result<ExitCode> {
    run_command(NativeCommand::Job(command))
}

fn run_command(command: NativeCommand<'_>) -> io::Result<ExitCode> {
    let result = (|| {
        // Public selectors only, never token/secret environment inputs.
        let tenant = required("FAB_RUST_TENANT")?;
        let account = required("FAB_RUST_ACCOUNT")?;
        let client = required("FAB_RUST_CLIENT_ID")?;
        run_selected(command, &tenant, &account, &client)
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
) -> Result<fabric_cli_output::text::TextPlan, Failure> {
    Err(failure(
        "unsupported",
        "Native read credentials currently require Windows.",
        1,
    ))
}

#[cfg(windows)]
fn run_selected(
    command: NativeCommand<'_>,
    tenant: &str,
    account: &str,
    client: &str,
) -> Result<fabric_cli_output::text::TextPlan, Failure> {
    let options = fabric_identity::LoginOptions::new(tenant, client, account)
        .map_err(|e| failure("invalid_configuration", e.to_string(), 2))?;
    let current = optional_path("FAB_RUST_CONTEXT", "/")?;
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
    if let NativeCommand::Job(command) = command {
        fabric_sdk_jobs::JobRequestHeaders::with_skill(command.skill.as_deref()).map_err(|_| {
            failure(
                "invalid_arguments",
                "Invalid job request header selection.",
                2,
            )
        })?;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failure("runtime_failed", "The read runtime could not start.", 1))?;
    runtime.block_on(async {
        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                match signal {
                    Ok(()) => Err(failure("cancelled", "Read cancelled.", 2)),
                    Err(_) => Err(failure("signal_failed", "The cancellation handler could not start.", 1)),
                }
            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(240)) => {
                Err(failure("timed_out", "The read command deadline elapsed.", 1))
            },
            result = execute(command, options, &current, home) => result,
        }
    })
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
fn optional_path(name: &str, default: &str) -> Result<String, Failure> {
    match std::env::var(name) {
        Ok(path) if path.starts_with('/') => Ok(path),
        Err(std::env::VarError::NotPresent) => Ok(default.into()),
        _ => Err(failure(
            "invalid_configuration",
            "The working context must be an absolute Fabric path.",
            2,
        )),
    }
}

#[cfg(windows)]
async fn execute(
    command: NativeCommand<'_>,
    options: fabric_identity::LoginOptions,
    current: &str,
    home: fabric_cli_context::HomeContext,
) -> Result<fabric_cli_output::text::TextPlan, Failure> {
    use fabric_cli::session::ReadSession;
    use fabric_cli_context::Element;
    use fabric_sdk::{Audience, Client, CredentialSelection};
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
    let sdk = Client::new(credential, selection);
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
    .map_err(|_| failure("transport_failed", "The HTTP executor could not start.", 1))?;
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
        config: fabric_cli::ReadConfig::default(),
    };
    let result = match command {
        NativeCommand::Read(command) => {
            session
                .with_source_operations(&executor)
                .dispatch(command, std::future::pending())
                .await
        }
        NativeCommand::Job(command) => {
            fabric_cli::jobs::JobReadSession {
                reads: &session,
                jobs: Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
            }
            .dispatch(command, std::future::pending())
            .await
        }
    };
    match result {
        Ok(plan) => Ok(plan),
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
