// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source deploy wrapper over injected files/UI and the native deployment engine.

use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, OutputStatus, Timestamp,
    text::{self, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_cli_transfer::{TransferCommand, TransferFiles, TransferKind, TransferUi};
use fabric_deploy::{
    deployment_config::{ConfigurationError, DeploymentConfig},
    notebook::DefinitionLimits,
    publish::{PublishError, PublishFailure, PublishedItem, publish_repository},
    repository::{NotebookRepository, RepositoryError},
    yaml::{self, YamlLimits, YamlPolicy},
};
use fabric_sdk::{Client, items::ItemExecutor};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use serde_json::{Map, Value};
use std::{
    fmt,
    future::{Future, poll_fn},
    path::Path,
    pin::pin,
    task::Poll,
};

/// Original failure kept independently of its sanitized stream projection.
#[derive(Debug)]
pub enum DeployFailure {
    /// Arguments, caller UI or file capability failure.
    Command(CommandError),
    /// Native source configuration validation or explicit unsupported branch.
    Configuration(ConfigurationError),
    /// Complete repository preflight failed before remote writes.
    Repository(RepositoryError),
    /// Discovery/operation failure with actual partial deployment evidence.
    Publish(PublishError),
}

/// Failed deploy retains its original stage and partial/uncertain service effects.
#[derive(Debug)]
pub struct DeployError {
    /// Actual failure, not a string-only projection.
    pub failure: DeployFailure,
}

impl fmt::Display for DeployError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Deployment failed; inspect original stage and completed/uncertain effects")
    }
}

impl std::error::Error for DeployError {}

/// Successful source wrapper output, including a distinct silent UI decline.
#[derive(Debug, Default)]
pub struct DeployResult {
    /// Actual completed create/update IDs in source execution order.
    pub completed: Vec<PublishedItem>,
    /// Confirmation was declined; no config reads or remote calls were made.
    pub declined: bool,
}

fn command(message: impl Into<String>) -> DeployError {
    DeployError {
        failure: DeployFailure::Command(CommandError::new(
            Some(message.into()),
            Some("DeploymentFailed".into()),
        )),
    }
}

/// Parse deploy's max-depth-one override tokens with JSON, never YAML typing.
///
/// The bounded lexical adapter rejects malformed/Unicode keys rather than silently
/// reproducing Python regex substring recovery. Dotted keys remain top-level.
///
/// # Errors
/// Rejects invalid keys, empty values, line breaks and unrepresentable JSON values.
pub fn parameters(tokens: &[String]) -> Result<Map<String, Value>, DeployError> {
    fn key_end(value: &str) -> Option<usize> {
        let equal = value.find('=')?;
        let key = value[..equal].strip_suffix('!').unwrap_or(&value[..equal]);
        (!key.is_empty()
            && key
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'.')))
        .then_some(equal)
    }
    let input = tokens.join(" ").replace('\\', "");
    let mut remaining = input.trim();
    if !tokens.is_empty() && remaining.is_empty() {
        return Err(command("Invalid deployment parameter format"));
    }
    let mut parameters = Map::new();
    while !remaining.is_empty() {
        let equal = key_end(remaining)
            .ok_or_else(|| command("Unsupported or invalid deployment parameter key"))?;
        let start = equal + 1;
        let end = remaining[start..]
            .char_indices()
            .find_map(|(offset, ch)| {
                (ch == ',' && key_end(remaining[start + offset + 1..].trim_start()).is_some())
                    .then_some(start + offset)
            })
            .unwrap_or(remaining.len());
        let value = &remaining[start..end];
        if value.is_empty() || value.contains(['\n', '\r']) {
            return Err(command("Invalid deployment parameter value"));
        }
        let value = match fabric_cli_output::parse_data(value) {
            Ok(value) => value,
            Err(_) if serde_json::from_str::<Value>(value).is_ok() => {
                return Err(command(
                    "Deployment parameter is outside the supported JSON domain",
                ));
            }
            Err(_) => Value::String(value.into()),
        };
        parameters.insert(remaining[..equal].into(), value);
        remaining = remaining.get(end + 1..).unwrap_or("").trim_start();
    }
    Ok(parameters)
}

/// Runtime-neutral deployment command; every external capability is injected.
pub struct DeployHost<'a, E> {
    /// Existing caller-owned SDK credential and selection.
    pub client: &'a Client,
    /// Existing shared request/paging/LRO adapter, optionally with source headers.
    pub executor: &'a E,
    /// Existing caller-confined file roots, listing and bounded read policy.
    pub files: &'a mut dyn TransferFiles,
    /// Existing caller confirmation and immediate-output policy.
    pub ui: &'a dyn TransferUi,
    /// Explicit finite local content bounds.
    pub definition_limits: DefinitionLimits,
    /// Explicit finite YAML syntax and alias-expansion bounds.
    pub yaml_limits: YamlLimits,
    /// Existing shared operation bounds, not a new polling policy.
    pub operation_limits: OperationLimits,
}

impl<E: OperationExecutor + ItemExecutor> DeployHost<'_, E> {
    /// Confirm, parse real YAML, validate overrides, preflight all files, deploy.
    ///
    /// Synchronous file preparation must run on a caller-appropriate worker.
    /// Libraries never prompt, print, discover credentials or initialize a runtime.
    ///
    /// # Errors
    /// Unsupported source fields fail before side effects; service failures retain
    /// actual completed IDs and the shared engine's accepted/uncertain operation.
    pub async fn execute(
        &mut self,
        args: &TransferCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DeployResult, DeployError> {
        if args.kind != TransferKind::Deploy {
            return Err(command("Expected a deploy invocation"));
        }
        let mut cancelled = pin!(cancelled);
        if !args.force {
            let environment = if args.target_env == "N/A" {
                "without a target environment".into()
            } else {
                format!("to target environment '{}'", args.target_env)
            };
            let question = format!(
                "Are you sure you want to deploy {environment} using the specified configuration file?"
            );
            let mut answer = pin!(self.ui.confirm(&question));
            let confirmed = poll_fn(|cx| {
                if cancelled.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Err(CommandError::new(
                        Some("Deployment cancelled".into()),
                        Some("LongRunningOperationCancelled".into()),
                    )));
                }
                answer.as_mut().poll(cx)
            })
            .await
            .map_err(|error| DeployError {
                failure: DeployFailure::Command(error),
            })?;
            if !confirmed {
                return Ok(DeployResult {
                    declined: true,
                    ..Default::default()
                });
            }
        }
        let cancelled_now = poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await;
        if cancelled_now {
            return Err(command("Deployment cancelled before configuration reads"));
        }
        if args.bulk_publish {
            return Err(DeployError {
                failure: DeployFailure::Configuration(ConfigurationError::Unsupported(
                    "command-scoped experimental bulk publication".into(),
                )),
            });
        }
        let mut params = parameters(args.params.as_deref().unwrap_or_default())?;
        // The source wrapper always overwrites this kwargs field; user input is
        // never forwarded into request metadata. The central adapter binds it.
        params.remove("host_app");
        let overrides = params.remove("config_override");
        if !params.is_empty() {
            return Err(DeployError {
                failure: DeployFailure::Configuration(ConfigurationError::Unsupported(
                    "additional deploy keyword parameters".into(),
                )),
            });
        }
        let path = Path::new(
            args.config
                .as_deref()
                .ok_or_else(|| command("Configuration path is required"))?,
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| command("Invalid configuration filename"))?;
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let config_root = self
            .files
            .resolve_directory(
                parent
                    .to_str()
                    .ok_or_else(|| command("Configuration path must be UTF-8"))?,
            )
            .map_err(|error| command(error.to_string()))?;
        let bytes = self
            .files
            .read(&config_root, name, self.yaml_limits.max_bytes)
            .map_err(|error| command(error.to_string()))?;
        let content =
            std::str::from_utf8(&bytes).map_err(|_| command("Configuration must be UTF-8"))?;
        let document =
            yaml::decode(content, YamlPolicy::Config, self.yaml_limits).map_err(|error| {
                DeployError {
                    failure: DeployFailure::Configuration(error),
                }
            })?;
        let config =
            DeploymentConfig::from_document(&document, &args.target_env, overrides.as_ref())
                .map_err(|error| DeployError {
                    failure: DeployFailure::Configuration(error),
                })?;
        config
            .require_notebook_publish()
            .map_err(|error| DeployError {
                failure: DeployFailure::Configuration(error),
            })?;
        let repository = config
            .workspace
            .get("repository_directory")
            .and_then(Value::as_str)
            .ok_or_else(|| command("Repository directory is required"))?;
        let repository = config_root.join(repository);
        let repository_root = self
            .files
            .resolve_directory(
                repository
                    .to_str()
                    .ok_or_else(|| command("Repository path must be UTF-8"))?,
            )
            .map_err(|error| command(error.to_string()))?;
        let repository = NotebookRepository::discover(
            self.files,
            &repository_root,
            self.definition_limits,
            self.yaml_limits.max_nodes,
        )
        .map_err(|error| DeployError {
            failure: DeployFailure::Repository(error),
        })?;
        let completed = publish_repository(
            self.client,
            self.executor,
            &config,
            repository,
            self.operation_limits,
            cancelled.as_mut(),
        )
        .await
        .map_err(|error| DeployError {
            failure: DeployFailure::Publish(error),
        })?;
        Ok(DeployResult {
            completed,
            declined: false,
        })
    }
}

/// Render source final message or silent confirmation decline using shared output.
///
/// # Errors
/// Shared text/JSON rendering failures remain explicit; they never replay writes.
pub fn render(
    result: &Result<DeployResult, DeployError>,
    timestamp: Timestamp,
    format: OutputFormat,
) -> Result<TextPlan, TextError> {
    if matches!(result, Ok(result) if result.declined) {
        return Ok(TextPlan {
            records: Vec::new(),
            exit_code: 0,
        });
    }
    let input = match result {
        Ok(_) => OutputInput {
            message: Some("Deployment completed successfully".into()),
            command: Some("deploy".into()),
            ..Default::default()
        },
        Err(error) => {
            let mut input = match &error.failure {
                DeployFailure::Publish(PublishError {
                    failure: PublishFailure::Operation(error),
                    ..
                }) => fabric_cli_mutations::operation_error_output(error),
                _ => OutputInput {
                    status: OutputStatus::Failure,
                    ..Default::default()
                },
            };
            input.error_code = Some("DeploymentFailed".into());
            input.message = Some(match &error.failure {
                DeployFailure::Configuration(error) => format!("Deployment failed: {error}"),
                DeployFailure::Repository(error) => format!("Deployment failed: {error}"),
                _ => "Deployment failed; inspect original stage and completed/uncertain effects"
                    .into(),
            });
            input.command = Some("deploy".into());
            input
        }
    };
    text::render(
        &OutputEnvelope::new(timestamp, input),
        format,
        &TextOptions::default(),
        &text::ascii_width,
        Presentation::default(),
    )
}
