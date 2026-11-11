// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parent-bound ML serving commands over the existing native request contracts.

use clap::{Arg, ArgAction, ArgGroup, ArgMatches, Command, error::ErrorKind};
use fabric_sdk::items::ml_models::{
    MlModelIdentity, MlModelRequest, MlModelRequests, MlModelResultKind, MlModelTarget,
    ScoreOutcome, UpdateEndpointRequest, UpdateEndpointVersionRequest,
};
use fabric_sdk_workspaces::mutations::{ResourceError, ResourceOperations, ResourceOutcome};
use serde_json::{Value, json};
use std::time::Duration;

#[path = "ml_endpoint_input.rs"]
mod input;
pub(crate) use input::diagnostic as input_diagnostic;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_SELECTOR_BYTES: usize = 4096;

/// A locally validated command; construction never consults identity or metadata.
#[derive(Clone)]
pub struct EndpointCommand {
    target: MlModelTarget,
    request: MlModelRequest,
    default_version: bool,
    score_input: Option<input::Input>,
    /// Overall native command deadline, including authentication and observation.
    pub timeout: Duration,
}

impl std::fmt::Debug for EndpointCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EndpointCommand")
            .field("request", &self.request)
            .field("default_version", &self.default_version)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Build the ML endpoint group without changing the existing artifact siblings.
pub fn command() -> Command {
    Command::new("ml-endpoint")
        .about("Native serving operations on an exact MLModel workspace/item pair")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("settings")
                .arg_required_else_help(true)
                .subcommand(target(Command::new("get").about("Get native endpoint settings")))
                .subcommand(body(mutation(target(
                    Command::new("patch").about("Patch native default-version settings"),
                )))),
        )
        .subcommand(
            Command::new("serving-version")
                .arg_required_else_help(true)
                .subcommand(
                    target(Command::new("list").about("Get ONE native page; retain continuation"))
                        .arg(Arg::new("continuation").long("continuation-token")),
                )
                .subcommand(version(
                    target(Command::new("get").about("Get an exact or explicitly selected default version")),
                    true,
                ))
                .subcommand(body(mutation(version(
                    target(Command::new("patch").about("Patch native serving-version scale settings")),
                    false,
                ))))
                .subcommands(["activate", "deactivate"].map(|name| {
                    waiting(mutation(version(target(Command::new(name)), false)))
                })),
        )
        .subcommand(waiting(mutation(target(
            Command::new("deactivate-all").about("Explicitly deactivate ALL serving versions"),
        ))))
        .subcommand(waiting(input::command(mutation(version(
            target(Command::new("score").about("Execute billable default or exact-version scoring")),
            true,
        )))))
        .after_help("IDs identify the owning MLModel, not a standalone endpoint. Reads never activate or score. List returns one bounded page, not an exhaustive collection. Mutations require --force (explicit effect approval); --wait observes a 202 without replaying it. JSON output preserves native fields. No name discovery, latest-version guessing, training or model upload is performed.")
}

fn target(command: Command) -> Command {
    command
        .arg(Arg::new("workspace").long("workspace-id").required(true))
        .arg(Arg::new("model").long("model-id").required(true))
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .default_value("240")
                .value_parser(clap::value_parser!(u64).range(1..=3600))
                .help("Overall deadline in seconds (1..3600)"),
        )
}

fn version(command: Command, allow_default: bool) -> Command {
    let command = command.arg(
        Arg::new("version")
            .long("version")
            .help("Exact opaque serving version; never inferred from registry order"),
    );
    if allow_default {
        command
            .arg(
                Arg::new("default")
                    .long("default")
                    .action(ArgAction::SetTrue)
                    .help("Explicitly use the configured default, with no latest fallback"),
            )
            .group(
                ArgGroup::new("selection")
                    .args(["version", "default"])
                    .required(true),
            )
    } else {
        command.mut_arg("version", |arg| arg.required(true))
    }
}

fn mutation(command: Command) -> Command {
    command.arg(
        Arg::new("force")
            .long("force")
            .required(true)
            .action(ArgAction::SetTrue)
            .help("Explicitly approve configuration, availability or compute effects"),
    )
}

fn body(command: Command) -> Command {
    command.arg(
        Arg::new("body")
            .long("body")
            .required(true)
            .help("Native JSON object, at most 1 MiB; unknown request fields are rejected"),
    )
}

fn waiting(command: Command) -> Command {
    command.arg(
        Arg::new("wait")
            .long("wait")
            .action(ArgAction::SetTrue)
            .help("Observe completion (and score results); acceptance alone is not completion"),
    )
}

/// Parse the mounted artifact leaf and validate everything before host selection.
///
/// # Errors
/// Rejects invalid targets, selectors, payloads, approval or output selection.
pub fn from_matches(matches: &ArgMatches) -> Result<EndpointCommand, clap::Error> {
    let invalid = || clap::Error::raw(ErrorKind::ValueValidation, super::parser::INVALID_ARGUMENTS);
    if matches
        .get_one::<String>("output_format")
        .is_some_and(|format| format != "json")
    {
        return Err(invalid());
    }
    let (_, artifact) = matches.subcommand().ok_or_else(invalid)?;
    let (_, endpoint) = artifact.subcommand().ok_or_else(invalid)?;
    let (group, values) = endpoint.subcommand().ok_or_else(invalid)?;
    let (action, values) = if matches!(group, "settings" | "serving-version") {
        values.subcommand().ok_or_else(invalid)?
    } else {
        (group, values)
    };
    let target = MlModelTarget::new(
        values.get_one::<String>("workspace").ok_or_else(invalid)?,
        values.get_one::<String>("model").ok_or_else(invalid)?,
        "MLModel",
    )
    .map_err(|_| invalid())?;
    let requests = MlModelRequests::new(MlModelIdentity::User);
    let version = values
        .try_get_one::<String>("version")
        .ok()
        .flatten()
        .map(String::as_str);
    let is_default = values
        .try_get_one::<bool>("default")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    let wait = values
        .try_get_one::<bool>("wait")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    let payload = || -> Result<Value, clap::Error> {
        let body = values.get_one::<String>("body").ok_or_else(invalid)?;
        if body.len() > MAX_BODY_BYTES {
            return Err(invalid());
        }
        fabric_cli_output::input::strict_json(body.as_bytes())
            .map_err(|_| input::error(input::JSON_MESSAGE))
    };
    if version.is_some_and(|version| version.len() > MAX_SELECTOR_BYTES) {
        return Err(invalid());
    }
    let mut score_input = None;
    let request = match (group, action) {
        ("settings", "get") => requests.get_endpoint(&target),
        ("settings", "patch") => {
            let payload: UpdateEndpointRequest =
                serde_json::from_value(payload()?).map_err(|_| invalid())?;
            if payload
                .default_version_name
                .as_ref()
                .is_some_and(|name| name.len() > MAX_SELECTOR_BYTES)
            {
                return Err(invalid());
            }
            requests.update_endpoint(&target, &payload)
        }
        ("serving-version", "list") => {
            let token = values.get_one::<String>("continuation");
            if token.is_some_and(|token| token.len() > MAX_SELECTOR_BYTES) {
                return Err(invalid());
            }
            requests.list_versions_page(&target, token.map(String::as_str))
        }
        ("serving-version", "get") if is_default => requests.get_endpoint(&target),
        ("serving-version", "get") => requests.get_version(&target, version.ok_or_else(invalid)?),
        ("serving-version", "patch") => {
            let payload: UpdateEndpointVersionRequest =
                serde_json::from_value(payload()?).map_err(|_| invalid())?;
            requests.update_version(&target, version.ok_or_else(invalid)?, &payload)
        }
        ("serving-version", "activate") => {
            requests.activate(&target, version.ok_or_else(invalid)?, wait)
        }
        ("serving-version", "deactivate") => {
            requests.deactivate(&target, version.ok_or_else(invalid)?, wait)
        }
        ("deactivate-all", _) => requests.deactivate_all(&target, wait),
        ("score", _) => {
            if let Some(payload) = input::raw(values)? {
                if is_default {
                    requests.score_default(&target, &payload, wait)
                } else {
                    requests.score_version(&target, version.ok_or_else(invalid)?, &payload, wait)
                }
            } else {
                score_input = Some(input::convenience(values, version, wait)?);
                if is_default {
                    requests.get_endpoint(&target)
                } else {
                    requests.get_version(&target, version.ok_or_else(invalid)?)
                }
            }
        }
        _ => return Err(invalid()),
    }
    .map_err(|_| invalid())?;
    Ok(EndpointCommand {
        target,
        request,
        default_version: group == "serving-version" && action == "get" && is_default,
        score_input,
        timeout: Duration::from_secs(*values.get_one::<u64>("timeout").ok_or_else(invalid)?),
    })
}

impl EndpointCommand {
    /// Local input path requiring one bounded read, before identity or dispatch.
    pub fn input_path(&self) -> Option<&str> {
        self.score_input.as_ref().and_then(input::Input::path)
    }

    /// Attach decoded bounded input without performing filesystem or service I/O.
    ///
    /// # Errors
    /// Rejects malformed/unsupported data files before any inference.
    pub fn prepare_input(&mut self, bytes: &[u8]) -> Result<(), ResourceError> {
        if let Some(input) = &mut self.score_input {
            input.prepare(bytes)?;
        }
        Ok(())
    }

    /// Whether this command can submit billable inference.
    pub fn is_score(&self) -> bool {
        self.score_input.is_some() || self.request.result_kind() == MlModelResultKind::Score
    }

    /// Safe target identifiers for a caller-owned diagnostic receipt.
    pub fn diagnostic_target(&self) -> Value {
        json!({"workspaceId":self.target.workspace_id(),"modelId":self.target.item_id()})
    }

    /// Execute on the caller's existing bounded operation policy, without retries.
    ///
    /// Default-version GET first reads endpoint settings, then reads the exact
    /// configured name. It never uses a fictitious `versions/default` API.
    ///
    /// # Errors
    /// Retains native status, service diagnostics, operation ID and uncertainty.
    pub async fn execute(
        &self,
        executor: &mut dyn ResourceOperations,
    ) -> Result<Value, ResourceError> {
        self.execute_inner(executor).await.map_err(|mut error| {
            if error.code == "InvalidResponse"
                && self.request.operation().method()
                    != fabric_sdk_workspaces::mutations::ResourceMethod::Get
            {
                error.mutation_may_have_occurred = true;
            }
            error
        })
    }

    async fn execute_inner(
        &self,
        executor: &mut dyn ResourceOperations,
    ) -> Result<Value, ResourceError> {
        if let Some(input) = &self.score_input {
            return self.execute_score_input(input, executor).await;
        }
        let outcome = self.request.execute(executor).await?;
        if self.default_version {
            let settings = completed_data(&outcome)?;
            validate_settings(settings)?;
            if settings
                .get("defaultVersionAssignmentBehavior")
                .and_then(Value::as_str)
                == Some("NotConfigured")
            {
                return Err(default_missing());
            }
            let version = settings
                .get("defaultVersionName")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty() && name.len() <= MAX_SELECTOR_BYTES)
                .ok_or_else(default_missing)?;
            let request =
                MlModelRequests::new(self.request.identity()).get_version(&self.target, version)?;
            let outcome = request.execute(executor).await?;
            let data = completed_data(&outcome)?;
            validate_version(data)?;
            return Ok(json!({
                "status": "completed", "httpStatus": 200, "data": data,
                "selection": {"mode": "default", "versionName": version}
            }));
        }
        match outcome {
            ResourceOutcome::Accepted { operation_id } => {
                if !self.request.expected_statuses().contains(&202) {
                    return Err(invalid_response(Some(202)));
                }
                Ok(json!({"status": "accepted", "httpStatus": 202, "operationId": operation_id}))
            }
            ResourceOutcome::Completed { status, data } => {
                if status != 200 {
                    return Err(invalid_response(Some(status)));
                }
                match self.request.result_kind() {
                    MlModelResultKind::Endpoint => {
                        validate_settings(
                            data.as_ref()
                                .ok_or_else(|| invalid_response(Some(status)))?,
                        )?;
                    }
                    MlModelResultKind::Version => {
                        validate_version(
                            data.as_ref()
                                .ok_or_else(|| invalid_response(Some(status)))?,
                        )?;
                    }
                    MlModelResultKind::VersionsPage => {
                        validate_page(
                            data.as_ref()
                                .ok_or_else(|| invalid_response(Some(status)))?,
                        )?;
                    }
                    MlModelResultKind::Score => {
                        ScoreOutcome::from_resource(ResourceOutcome::Completed {
                            status,
                            data: data.clone(),
                        })?;
                    }
                    MlModelResultKind::None => {}
                    MlModelResultKind::Item => return Err(invalid_response(Some(status))),
                }
                Ok(json!({"status": "completed", "httpStatus": status, "data": data}))
            }
        }
    }

    async fn execute_score_input(
        &self,
        input: &input::Input,
        executor: &mut dyn ResourceOperations,
    ) -> Result<Value, ResourceError> {
        if self.input_path().is_some() {
            return Err(input::local_error(
                "ScoreInputNotPrepared",
                "Local input must be prepared before service dispatch; no inference was submitted.",
            ));
        }
        let requests = MlModelRequests::new(self.request.identity());
        let outcome = self.request.execute(executor).await?;
        let data = completed_data(&outcome)?;
        let version = if let Some(version) = &input.version {
            version.clone()
        } else {
            validate_settings(data)?;
            if data
                .get("defaultVersionAssignmentBehavior")
                .and_then(Value::as_str)
                == Some("NotConfigured")
            {
                return Err(default_missing());
            }
            data.get("defaultVersionName")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty() && name.len() <= MAX_SELECTOR_BYTES)
                .ok_or_else(default_missing)?
                .to_owned()
        };
        let metadata_outcome;
        let metadata = if input.version.is_some() {
            data
        } else {
            metadata_outcome = requests
                .get_version(&self.target, &version)?
                .execute(executor)
                .await?;
            completed_data(&metadata_outcome)?
        };
        validate_version(metadata)?;
        if metadata.get("versionName").and_then(Value::as_str) != Some(version.as_str()) {
            return Err(invalid_response(Some(200)));
        }
        let payload = input.payload(metadata)?;
        let request = requests.score_version(&self.target, &version, &payload, input.wait)?;
        let outcome = request.execute(executor).await?;
        match ScoreOutcome::from_resource(outcome)? {
            ScoreOutcome::Accepted { operation_id } => Ok(
                json!({"status":"accepted","httpStatus":202,"operationId":operation_id,
                        "selection":{"mode":if input.version.is_some() {"exact"} else {"default"},"versionName":version}}),
            ),
            ScoreOutcome::Completed(data) => {
                Ok(json!({"status":"completed","httpStatus":200,"data":data,
                        "selection":{"mode":if input.version.is_some() {"exact"} else {"default"},"versionName":version}}))
            }
        }
    }
}

fn completed_data(outcome: &ResourceOutcome) -> Result<&Value, ResourceError> {
    match outcome {
        ResourceOutcome::Completed {
            status: 200,
            data: Some(data),
        } => Ok(data),
        ResourceOutcome::Completed { status, .. } => Err(invalid_response(Some(*status))),
        ResourceOutcome::Accepted { .. } => Err(invalid_response(Some(202))),
    }
}

fn invalid_response(status: Option<u16>) -> ResourceError {
    ResourceError {
        code: "InvalidResponse".into(),
        message: "Invalid native ML endpoint response".into(),
        status,
        ..ResourceError::input()
    }
}

fn default_missing() -> ResourceError {
    ResourceError {
        code: "DefaultNotConfigured".into(),
        message: "The ML endpoint has no configured default serving version".into(),
        ..ResourceError::input()
    }
}

fn object(value: &Value) -> Result<(), ResourceError> {
    if !value.is_object() {
        return Err(invalid_response(Some(200)));
    }
    Ok(())
}

fn strings(value: &Value, fields: &[&str]) -> Result<(), ResourceError> {
    object(value)?;
    for field in fields {
        if value
            .get(field)
            .is_some_and(|value| !value.is_string() && !value.is_null())
        {
            return Err(invalid_response(Some(200)));
        }
    }
    Ok(())
}

fn validate_settings(value: &Value) -> Result<(), ResourceError> {
    strings(
        value,
        &["defaultVersionAssignmentBehavior", "defaultVersionName"],
    )?;
    if let Some(info) = value
        .get("defaultVersionInfo")
        .filter(|info| !info.is_null())
    {
        validate_version(info)?;
    }
    Ok(())
}

fn validate_version(value: &Value) -> Result<(), ResourceError> {
    strings(
        value,
        &[
            "versionName",
            "status",
            "scaleRule",
            "lastStateChangeDateTime",
        ],
    )?;
    if let Some(details) = value
        .get("failureDetails")
        .filter(|details| !details.is_null())
    {
        strings(details, &["errorCode", "message"])?;
    }
    for field in ["inputSignature", "outputSignature"] {
        if let Some(signature) = value.get(field).filter(|value| !value.is_null()) {
            let signature = signature
                .as_array()
                .ok_or_else(|| invalid_response(Some(200)))?;
            for entry in signature {
                strings(entry, &["name", "type"])?;
                if entry
                    .get("required")
                    .is_some_and(|value| !value.is_boolean())
                {
                    return Err(invalid_response(Some(200)));
                }
            }
        }
    }
    Ok(())
}

fn validate_page(value: &Value) -> Result<(), ResourceError> {
    strings(value, &["continuationToken", "continuationUri"])?;
    let entries = value
        .get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response(Some(200)))?;
    for entry in entries {
        validate_version(entry)?;
    }
    Ok(())
}
