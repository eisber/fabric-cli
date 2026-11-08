// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in graph-only refresh arguments and caller-injected delivery.

use clap::{Arg, ArgAction, error::ErrorKind};
use fabric_sdk::{
    Client,
    observe::ObserveOptions,
    v2::{ApprovalPolicy, PreparedOperation},
};
use fabric_sdk_graph::{self as refresh, GraphRefresh, GraphRefreshError, GraphRefreshErrorKind};
use fabric_sdk_jobs::{JobClient, JobError, JobLimits, graph_refresh::RefreshReceipt};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::Future,
    io::{self, Write},
    time::Duration,
};

/// Explicit target, total deadline and per-invocation approval.
#[derive(Clone, Debug)]
pub struct RefreshCommand {
    /// ID-qualified GraphModel runtime/graph path; absent only for local schema.
    pub path: Option<String>,
    /// Whether to observe the accepted native job to terminal state.
    pub wait: bool,
    /// Caller-chosen total local budget, not a server timeout or remote cancel.
    pub timeout: Option<Duration>,
    /// Local schema discovery, without identity or networking.
    pub schema: bool,
    /// Explicit metered execution approval for this invocation only.
    pub approve: bool,
    /// Explicit Preview capability opt-in.
    pub preview: bool,
}

/// The operation-only schema; no source job flags or query fallback.
pub fn command() -> clap::Command {
    clap::Command::new("refresh")
        .about("Refresh one GraphModel runtime graph (opt-in Preview)")
        .after_help("Target: /<workspace-id>.Workspace/<graph-id>.GraphModel/runtime/graph. Requires --preview --approve and an explicit total --timeout in seconds. No request body, other artifact support, metadata/query fallback or automatic cancellation. HTTP 202 means accepted, not refreshed.")
        .arg(Arg::new("path").required_unless_present("schema"))
        .arg(Arg::new("schema").long("schema").action(ArgAction::SetTrue).conflicts_with_all(["path","wait","timeout","approve","preview"]))
        .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue))
        .arg(Arg::new("timeout").long("timeout").value_parser(clap::value_parser!(u64)).required_unless_present("schema"))
        .arg(Arg::new("approve").long("approve").action(ArgAction::SetTrue))
        .arg(Arg::new("preview").long("preview").action(ArgAction::SetTrue))
}

/// Parse arguments starting with refresh, before accessing identity or runtime.
///
/// # Errors
/// Returns sanitized errors for unsupported paths, fields and finite deadlines.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<RefreshCommand, clap::Error> {
    let matches = command().try_get_matches_from(args)?;
    let schema = matches.get_flag("schema");
    let path = matches.get_one::<String>("path").cloned();
    if let Some(path) = &path {
        refresh::target(path).map_err(|_| invalid())?;
    }
    let timeout = matches
        .get_one::<u64>("timeout")
        .copied()
        .map(Duration::from_secs);
    if timeout.is_some_and(|timeout| timeout.is_zero() || timeout > Duration::from_secs(86_400)) {
        return Err(invalid());
    }
    Ok(RefreshCommand {
        path,
        timeout,
        schema,
        wait: matches.get_flag("wait"),
        approve: matches.get_flag("approve"),
        preview: matches.get_flag("preview"),
    })
}

fn invalid() -> clap::Error {
    clap::Error::raw(
        ErrorKind::InvalidValue,
        "Refresh requires an ID-only GraphModel runtime/graph path and an explicit positive timeout of at most 86400 seconds.",
    )
}

/// Invocation-bound policy; only the exact prepared graph effect is approved.
pub struct Policy<'a>(pub &'a RefreshCommand);

impl ApprovalPolicy for Policy<'_> {
    type Error = &'static str;

    fn authorize(&mut self, prepared: &PreparedOperation) -> Result<(), Self::Error> {
        if !self.0.approve
            || !self.0.preview
            || self.0.path.as_deref() != Some(prepared.target().canonical_path())
            || prepared.operation_id() != refresh::OPERATION_ID
            || prepared.effects() != refresh::EFFECTS
        {
            return Err(
                "Explicit Preview and metered graph refresh approval are required; no refresh was submitted.",
            );
        }
        Ok(())
    }
}

/// A once-delivered JSON receipt, never permission to replay after output failure.
pub struct Delivery {
    /// Stable envelope, with safe classifications rather than raw service messages.
    pub value: Value,
    /// Accepted/completed 0; native/observation/output failure 1; invalid input 2.
    pub exit: u8,
}

impl Delivery {
    /// Write and flush once to the correct stream, preserving evidence on failure.
    pub fn write_to(&self, stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
        let output = if self.exit == 0 {
            &mut *stdout
        } else {
            &mut *stderr
        };
        let result = serde_json::to_writer(&mut *output, &self.value)
            .map_err(io::Error::other)
            .and_then(|()| writeln!(output))
            .and_then(|()| output.flush());
        if result.is_ok() {
            return self.exit;
        }
        let mut value = self.value.clone();
        value["status"] = json!("unknown");
        value["error"] = json!({"code":"OutputFailure","message":"Receipt delivery failed; do not replay graph refresh."});
        let _ = serde_json::to_writer(&mut *stderr, &value);
        let _ = writeln!(stderr);
        let _ = stderr.flush();
        1
    }
}

fn receipt(
    path: &str,
    submission: Option<&RefreshReceipt>,
    uncertain: bool,
    code: Option<&str>,
) -> Value {
    let run = submission.and_then(RefreshReceipt::run);
    json!({
        "schemaVersion":"0.1",
        "status":if code.is_some() { if uncertain { "unknown" } else { "failed" } } else if run.is_some() { "accepted" } else { "completed" },
        "data":null,
        "meta":{
            "operation":refresh::OPERATION_ID,
            "target":path,
            "handle":run.map(|run| json!({
                "kind":"itemJobRun","workspaceId":submission.unwrap().target().workspace_id(),
                "itemId":submission.unwrap().target().graph_id(),"instanceId":run.id()
            })),
            "responseStatus":submission.map(|submission| submission.response().status),
            "accepted":run.is_some(),
            "mutationMayHaveOccurred":uncertain,
            "effects":refresh::EFFECTS,
            "effectsCompleteness":"unknown",
            "remoteCancelled":false,"remoteReplayed":false
        },
        "error":code.map(|code| json!({"code":code,"message":"Graph refresh failed or remains incomplete; retain its native evidence."}))
    })
}

fn success(path: &str, result: &GraphRefresh) -> Delivery {
    let mut value = receipt(path, Some(&result.submission), true, None);
    if let Some(observation) = &result.observation {
        value["status"] = json!("completed");
        value["meta"]["lastState"] = json!("Completed");
        value["meta"]["statusRequests"] = json!(observation.requests);
    }
    Delivery { value, exit: 0 }
}

fn error(path: &str, error: &GraphRefreshError) -> Delivery {
    let (code, uncertain) = match &error.source {
        GraphRefreshErrorKind::InvalidInput => ("InvalidInput", false),
        GraphRefreshErrorKind::Submission(error) => {
            let code = match &error.source {
                JobError::Credential => "Credential",
                JobError::Cancelled => "Cancelled",
                JobError::InvalidAudience | JobError::InvalidEndpoint => "InvalidContext",
                _ => "SubmissionFailed",
            };
            (code, error.mutation_may_have_occurred)
        }
        GraphRefreshErrorKind::InitialWait(JobError::Cancelled) => ("Cancelled", true),
        GraphRefreshErrorKind::InitialWait(_) => ("WaitFailed", true),
        GraphRefreshErrorKind::DelayLimit => ("DelayLimit", true),
        GraphRefreshErrorKind::Observation(_) => ("ObservationIncomplete", true),
    };
    let mut value = receipt(path, error.submission.as_ref(), uncertain, Some(code));
    if let GraphRefreshErrorKind::Submission(error) = &error.source {
        value["meta"]["responseStatus"] =
            json!(error.response.as_ref().map(|response| response.status));
        value["meta"]["responseBodyOmitted"] = json!(error.response_body_omitted);
    }
    if let GraphRefreshErrorKind::Observation(error) = &error.source {
        value["meta"]["statusRequests"] = json!(error.requests);
        if let Some(last) = &error.last_observation {
            let state = match last.raw_status.get("status").and_then(Value::as_str) {
                Some(
                    state @ ("Failed" | "Cancelled" | "Deduped" | "NotStarted" | "InProgress"),
                ) => state,
                _ => "unknown",
            };
            value["meta"]["lastState"] = json!(state);
            if matches!(state, "Failed" | "Cancelled" | "Deduped") {
                value["status"] = json!("failed");
            }
        }
    }
    Delivery {
        value,
        exit: if code == "InvalidInput" { 2 } else { 1 },
    }
}

/// Prepare, approve, submit once, and optionally observe using injected SDK hosts.
///
/// # Errors
/// Preparation/policy refusals precede dispatch; native failures return evidence.
pub async fn dispatch(
    command: &RefreshCommand,
    client: &Client,
    jobs: &JobClient,
    executor: &dyn OperationExecutor,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Delivery, &'static str> {
    let path = command
        .path
        .as_deref()
        .ok_or("A graph refresh path is required.")?;
    if command.schema
        || command
            .timeout
            .is_none_or(|timeout| timeout.is_zero() || timeout > Duration::from_secs(86_400))
    {
        return Err("Invalid graph refresh execution input.");
    }
    let prepared = refresh::prepare(path).map_err(|_| "Invalid graph refresh target.")?;
    let approved = prepared.authorize(&mut Policy(command))?;
    let limits = OperationLimits::default();
    let options = command.wait.then_some(ObserveOptions {
        wait: true,
        result: false,
        max_requests: limits.max_requests,
        max_response_bytes: JobLimits::default().max_response_bytes,
        max_delay: limits.max_poll_delay,
    });
    match refresh::execute(&approved, client, jobs, executor, options, cancelled).await {
        Ok(result) => Ok(success(path, &result)),
        Err(failure) => Ok(error(path, &failure)),
    }
}
