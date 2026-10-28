// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source deploy wrapper over injected files/UI and the native deployment engine.

use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, OutputStatus, Timestamp,
    text::{self, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_cli_transfer::{
    TransferCommand, TransferEvent, TransferFiles, TransferKind, TransferUi,
};
use fabric_deploy::{
    deployment_config::{
        CommandConfiguration, CommandOptions, ConfigurationError, DeploymentConfig,
    },
    notebook::DefinitionLimits,
    parameter_transforms::{
        ParameterFileHost, TransformBudget, TransformError, TransformLimits,
        expand_parameter_environment,
    },
    parameters::ParameterNotice,
    planner::EngineContext,
    publish::{PublishError, PublishFailure, PublishedItem, publish_repository},
    repository::{DeploymentRepository, RepositoryError},
    workflow::{WorkflowError, WorkflowFailure, WorkflowResult, deploy_repository},
    yaml::{self, YamlLimits, YamlPolicy},
};
use fabric_sdk::{Client, items::ItemExecutor};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use serde_json::{Map, Value};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fmt,
    future::{Future, poll_fn},
    path::{Path, PathBuf},
    pin::{Pin, pin},
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
    /// Broad workflow failure with original completed/accepted effects.
    Workflow(Box<WorkflowError>),
    /// Immediate UI delivery failed after confirmed remote work.
    Output {
        /// Original caller output failure.
        error: CommandError,
        /// Actual completed/accepted effects, never replayed to repair output.
        partial: Box<WorkflowResult>,
    },
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

/// Explicit process inputs for parameter loading; libraries do not discover them.
#[derive(Clone, Default)]
pub struct DeployInputs {
    /// Caller-provided environment values, used only under the frozen source flag.
    pub environment: BTreeMap<String, String>,
}

impl fmt::Debug for DeployInputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeployInputs")
            .field("environment", &"<redacted>")
            .field("environment_entries", &self.environment.len())
            .finish()
    }
}

/// Validated command snapshot plus caller-resolved, confined local paths.
pub struct LoadedDeploymentConfiguration {
    /// The configuration owner's immutable complete command snapshot.
    pub configuration: CommandConfiguration,
    /// Actual confined repository root returned by the existing file capability.
    pub repository_root: PathBuf,
}

impl LoadedDeploymentConfiguration {
    fn context(
        &self,
        limits: DefinitionLimits,
        inputs: &DeployInputs,
    ) -> Result<EngineContext, DeployError> {
        let mut context = EngineContext::from_configuration(self.configuration.clone())
            .map_err(configuration)?
            .with_definition_limits(limits)
            .with_repository_root(
                self.repository_root
                    .to_str()
                    .ok_or_else(|| command("Repository path must be UTF-8"))?,
            );
        let mut features = vec!["disable_print_identity".into()];
        if self.configuration.feature_enabled("enable_bulk_publish") {
            features.extend([
                "enable_experimental_features".into(),
                "enable_bulk_publish".into(),
            ]);
        }
        context = context.with_scope_features(features);
        context.environment_variables = inputs
            .environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Ok(context)
    }
}
struct Templates<'a> {
    values: &'a [(String, Option<Value>)],
    cursor: Cell<usize>,
    requested: RefCell<Option<String>>,
}

impl ParameterFileHost for Templates<'_> {
    fn template(&self, reference: &str) -> Result<Option<Value>, TransformError> {
        let index = self.cursor.get();
        self.cursor.set(index + 1);
        if let Some((expected, value)) = self.values.get(index) {
            if expected != reference {
                return Err(TransformError::Invalid(
                    "Template resolution order changed".into(),
                ));
            }
            return Ok(value.clone());
        }
        self.requested.replace(Some(reference.into()));
        Err(TransformError::Invalid(
            "Template file capability handoff".into(),
        ))
    }
}

fn configuration(error: ConfigurationError) -> DeployError {
    DeployError {
        failure: DeployFailure::Configuration(error),
    }
}

fn parameter_document(
    bytes: &[u8],
    enabled: bool,
    inputs: &DeployInputs,
    limits: YamlLimits,
) -> Result<Value, DeployError> {
    let raw = std::str::from_utf8(bytes).map_err(|_| command("Parameter file must be UTF-8"))?;
    let expanded = expand_parameter_environment(
        raw,
        enabled,
        &inputs.environment,
        &mut TransformBudget::new(
            TransformLimits {
                max_bytes: limits.max_bytes,
                ..Default::default()
            },
            &|| false,
        ),
    )
    .map_err(|error| configuration(ConfigurationError::Transform(Box::new(error))))?;
    yaml::decode(&expanded, YamlPolicy::Parameter, limits).map_err(configuration)
}

fn legacy_parameters(
    program: Option<&fabric_deploy::parameter_transforms::ParameterProgram>,
) -> bool {
    fn dynamic(value: &Value) -> bool {
        match value {
            Value::String(value) => value.starts_with('$'),
            Value::Array(values) => values.iter().any(dynamic),
            Value::Object(values) => values.values().any(dynamic),
            _ => false,
        }
    }
    program.is_none_or(|program| {
        program.item_settings().is_empty()
            && !dynamic(program.source_document())
            && ["find_replace", "key_value_replace"].iter().all(|section| {
                program
                    .source_document()
                    .get(*section)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .all(|rule| {
                        ["item_type", "item_name", "file_path"]
                            .iter()
                            .all(|key| rule.get(*key).is_none())
                    })
            })
    })
}

fn command(message: impl Into<String>) -> DeployError {
    DeployError {
        failure: DeployFailure::Command(CommandError::new(
            Some(message.into()),
            Some("DeploymentFailed".into()),
        )),
    }
}

async fn file_phase<T, C: Future<Output = ()> + ?Sized>(
    work: impl Future<Output = Result<T, DeployError>>,
    mut cancelled: Pin<&mut C>,
) -> Result<T, DeployError> {
    let mut work = pin!(work);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(DeployError {
                failure: DeployFailure::Command(CommandError::new(
                    Some("Deployment cancelled during local preparation".into()),
                    Some("LongRunningOperationCancelled".into()),
                )),
            }));
        }
        work.as_mut().poll(cx)
    })
    .await
}

async fn read_file(
    files: &mut dyn TransferFiles,
    path: &Path,
    max_bytes: usize,
) -> Result<(Vec<u8>, PathBuf), DeployError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| command("Invalid deployment input filename"))?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let root = files
        .resolve_directory_async(
            parent
                .to_str()
                .ok_or_else(|| command("Deployment input path must be UTF-8"))?,
        )
        .await
        .map_err(|error| command(error.to_string()))?;
    let bytes = files
        .read_async(&root, name, max_bytes)
        .await
        .map_err(|error| command(error.to_string()))?;
    Ok((bytes, root))
}

async fn template_bytes(
    files: &mut dyn TransferFiles,
    path: &Path,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, DeployError> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Some(parent) = parent.to_str() else {
        return Ok(None);
    };
    let root = match files.resolve_directory_async(parent).await {
        Ok(root) => root,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(command(error.to_string())),
    };
    match files.read_async(&root, name, max_bytes).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(command(error.to_string())),
    }
}

/// Render a source environment warning with the existing Python-style formatter.
///
/// # Errors
/// Retains shared formatting failures rather than silently dropping the notice.
pub fn environment_warning(
    warning: &fabric_deploy::deployment_config::EnvironmentWarning,
) -> Result<String, TextError> {
    let available = text::python_string(&Value::Array(
        warning
            .available
            .iter()
            .cloned()
            .map(Value::String)
            .collect(),
    ))?;
    Ok(format!(
        "Environment '{}' not found in '{}'. Available environments: {available}. This setting will be skipped.",
        warning.environment, warning.field
    ))
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
    /// OS-backed file preparation runs on the caller's existing bounded worker.
    /// Legacy synchronous injected adapters retain their explicit worker contract.
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
        self.execute_with_inputs(args, &DeployInputs::default(), cancelled)
            .await
    }

    /// Execute with explicit environment inputs for source parameter expansion.
    ///
    /// # Errors
    /// Local/feature failures precede writes; broad service failures retain effects.
    pub async fn execute_with_inputs(
        &mut self,
        args: &TransferCommand,
        inputs: &DeployInputs,
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
            self.ui.emit(TransferEvent::Warning(
                "Experimental bulk publish is enabled: items will be deployed through the source bulk import workflow.".into(),
            )).map_err(|error|DeployError {failure:DeployFailure::Command(error)})?;
        }
        let mut params = parameters(args.params.as_deref().unwrap_or_default())?;
        // The source wrapper always overwrites this kwargs field; user input is
        // never forwarded into request metadata. The central adapter binds it.
        params.insert(
            "host_app".into(),
            format!("ms-fabric-cli/{}", env!("CARGO_PKG_VERSION")).into(),
        );
        let overrides = params.remove("config_override");
        let options = CommandOptions {
            bulk_publish: args.bulk_publish,
            kwargs: params,
            features: vec!["disable_print_identity".into()],
            ..Default::default()
        };
        let path = Path::new(
            args.config
                .as_deref()
                .ok_or_else(|| command("Configuration path is required"))?,
        );
        let (bytes, config_root) = file_phase(
            read_file(self.files, path, self.yaml_limits.max_bytes),
            cancelled.as_mut(),
        )
        .await?;
        let content =
            std::str::from_utf8(&bytes).map_err(|_| command("Configuration must be UTF-8"))?;
        let document =
            yaml::decode(content, YamlPolicy::Config, self.yaml_limits).map_err(|error| {
                DeployError {
                    failure: DeployFailure::Configuration(error),
                }
            })?;
        let mut config =
            DeploymentConfig::from_document(&document, &args.target_env, overrides.as_ref())
                .map_err(|error| DeployError {
                    failure: DeployFailure::Configuration(error),
                })?;
        for warning in &config.warnings {
            let warning =
                environment_warning(warning).map_err(|error| command(error.to_string()))?;
            self.ui
                .emit(TransferEvent::Warning(warning))
                .map_err(|error| DeployError {
                    failure: DeployFailure::Command(error),
                })?;
        }
        if let Some(parameter) = config
            .workspace
            .get("parameter_file_path")
            .and_then(Value::as_str)
        {
            let (bytes, parameter_root) = file_phase(
                read_file(
                    self.files,
                    &config_root.join(parameter),
                    self.yaml_limits.max_bytes,
                ),
                cancelled.as_mut(),
            )
            .await?;
            let enabled = config
                .command_configuration(options.clone())
                .map_err(configuration)?
                .feature_enabled("enable_environment_variable_replacement");
            let document = parameter_document(&bytes, enabled, inputs, self.yaml_limits)?;
            let mut templates = Vec::new();
            let mut budget = TransformBudget::new(
                TransformLimits {
                    max_bytes: self.yaml_limits.max_bytes,
                    ..Default::default()
                },
                &|| false,
            );
            loop {
                let (result, requested) = {
                    let host = Templates {
                        values: &templates,
                        cursor: Cell::new(0),
                        requested: RefCell::new(None),
                    };
                    let result = config.resolve_parameters_with_host(
                        &document,
                        &host,
                        options.clone(),
                        &mut budget,
                    );
                    (result, host.requested.into_inner())
                };
                let Some(reference) = requested else {
                    let prepared = result.map_err(configuration)?;
                    for skipped in prepared.skipped_templates {
                        self.ui
                            .emit(TransferEvent::Warning(format!(
                                "Parameter template was skipped: {skipped}"
                            )))
                            .map_err(|error| DeployError {
                                failure: DeployFailure::Command(error),
                            })?;
                    }
                    break;
                };
                let bytes = file_phase(
                    template_bytes(
                        self.files,
                        &parameter_root.join(&reference),
                        self.yaml_limits.max_bytes,
                    ),
                    cancelled.as_mut(),
                )
                .await?;
                let value = match bytes {
                    None => None,
                    Some(bytes) => {
                        match parameter_document(&bytes, enabled, inputs, self.yaml_limits) {
                            Ok(value) => Some(value),
                            Err(error)
                                if matches!(
                                    &error.failure,
                                    DeployFailure::Configuration(ConfigurationError::Limit)
                                        | DeployFailure::Configuration(
                                            ConfigurationError::Transform(_)
                                        )
                                ) =>
                            {
                                return Err(error);
                            }
                            Err(_) => None,
                        }
                    }
                };
                templates.push((reference, value));
            }
            for notice in config
                .parameters()
                .expect("explicitly resolved parameter file")
                .notices()
            {
                match notice {
                    ParameterNotice::RegexIgnored => {
                        self.ui.emit(TransferEvent::Warning(
                            "The provided is_regex value is not set to 'true', regex matching will be ignored.".into(),
                        )).map_err(|error| DeployError { failure: DeployFailure::Command(error) })?;
                    }
                    ParameterNotice::AllEnvironment { key, replacement } => {
                        for message in [
                            format!("Found the reserved environment key '{key}'"),
                            format!(
                                "The replace value: '{replacement}' will be applied for any target environment"
                            ),
                        ] {
                            self.ui
                                .emit(TransferEvent::Warning(message))
                                .map_err(|error| DeployError {
                                    failure: DeployFailure::Command(error),
                                })?;
                        }
                    }
                }
            }
        }
        let snapshot = config
            .command_configuration(options)
            .map_err(configuration)?;
        snapshot.require_no_extensions().map_err(configuration)?;
        snapshot
            .validate_operation_features()
            .map_err(configuration)?;
        let legacy = !args.bulk_publish
            && config.require_notebook_publish().is_ok()
            && legacy_parameters(snapshot.parameters());
        let repository = config_root.join(snapshot.repository_directory());
        let repository_path = repository
            .to_str()
            .ok_or_else(|| command("Repository path must be UTF-8"))?;
        let repository_root = file_phase(
            async {
                self.files
                    .resolve_directory_async(repository_path)
                    .await
                    .map_err(|error| command(error.to_string()))
            },
            cancelled.as_mut(),
        )
        .await?;
        let loaded = LoadedDeploymentConfiguration {
            configuration: snapshot,
            repository_root,
        };
        if loaded.configuration.publish().skip() && loaded.configuration.unpublish().skip() {
            if matches!(
                loaded.configuration.target(),
                fabric_deploy::deployment_config::WorkspaceTarget::Name(_)
            ) {
                deploy_repository(
                    self.client,
                    self.executor,
                    &config,
                    DeploymentRepository::default(),
                    loaded.context(self.definition_limits, inputs)?,
                    self.operation_limits,
                    cancelled.as_mut(),
                )
                .await
                .map_err(|error| DeployError {
                    failure: DeployFailure::Workflow(Box::new(error)),
                })?;
            }
            return Ok(DeployResult::default());
        }
        if legacy {
            let repository = file_phase(
                async {
                    self.files
                        .prepare_repository_async(
                            &loaded.repository_root,
                            self.definition_limits,
                            self.yaml_limits.max_nodes,
                        )
                        .await
                        .map_err(|error| DeployError {
                            failure: DeployFailure::Repository(error),
                        })
                },
                cancelled.as_mut(),
            )
            .await;
            match repository {
                Ok(repository) => {
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
                    return Ok(DeployResult {
                        completed,
                        declined: false,
                    });
                }
                Err(DeployError {
                    failure:
                        DeployFailure::Repository(RepositoryError::Configuration(
                            ConfigurationError::Unsupported(_),
                        )),
                }) => {}
                Err(DeployError {
                    failure:
                        DeployFailure::Repository(RepositoryError::Notebook(
                            fabric_deploy::notebook::DeploymentError::UnsupportedContent,
                        )),
                }) => {}
                Err(error) => return Err(error),
            }
        }
        let repository = file_phase(
            async {
                self.files
                    .prepare_deployment_repository_async(
                        &loaded.repository_root,
                        self.definition_limits,
                        self.yaml_limits.max_nodes,
                    )
                    .await
                    .map_err(|error| DeployError {
                        failure: DeployFailure::Repository(error),
                    })
            },
            cancelled.as_mut(),
        )
        .await?;
        let context = loaded.context(self.definition_limits, inputs)?;
        let mut completed = deploy_repository(
            self.client,
            self.executor,
            &config,
            repository,
            context,
            self.operation_limits,
            cancelled.as_mut(),
        )
        .await
        .map_err(|error| DeployError {
            failure: DeployFailure::Workflow(Box::new(error)),
        })?;
        for notice in std::mem::take(&mut completed.notices) {
            if let Err(error) = self.ui.emit(TransferEvent::Warning(notice)) {
                return Err(DeployError {
                    failure: DeployFailure::Output {
                        error,
                        partial: Box::new(completed),
                    },
                });
            }
        }
        Ok(DeployResult {
            completed: completed.published,
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
                DeployFailure::Workflow(error) => match &error.failure {
                    WorkflowFailure::Operation(error) => {
                        fabric_cli_mutations::operation_error_output(error)
                    }
                    _ => OutputInput {
                        status: OutputStatus::Failure,
                        ..Default::default()
                    },
                },
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
