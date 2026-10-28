// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-pinned deployment configuration validation, without ambient state or I/O.

use crate::config::{extract_workspace_settings, get_config_value};
use serde_json::{Map, Value};
use std::fmt;

mod model;
pub use model::{
    CommandConfiguration, CommandConfigurationData, CommandOptions, OperationSettings,
    WorkspaceTarget,
};

/// Exact CICD serial publication order, independent of user scope ordering.
pub const ITEM_TYPES: &[&str] = &[
    "VariableLibrary",
    "Warehouse",
    "MirroredDatabase",
    "Lakehouse",
    "SQLDatabase",
    "CosmosDBDatabase",
    "Environment",
    "UserDataFunction",
    "Eventhouse",
    "SparkJobDefinition",
    "Notebook",
    "SemanticModel",
    "Report",
    "PaginatedReport",
    "CopyJob",
    "DataBuildToolJob",
    "KQLDatabase",
    "KQLQueryset",
    "Dataflow",
    "DataPipeline",
    "Reflex",
    "Eventstream",
    "KQLDashboard",
    "GraphQLApi",
    "ApacheAirflowJob",
    "MountedDataFactory",
    "Ontology",
    "DataAgent",
    "MLExperiment",
    "Map",
    "GraphModel",
    "OrgApp",
    "OrgAppAudience",
];

const CORE: &[&str] = &[
    "workspace_id",
    "workspace",
    "repository_directory",
    "item_types_in_scope",
    "parameter",
];
const PUBLISH: &[&str] = &[
    "exclude_regex",
    "folder_exclude_regex",
    "folder_path_to_include",
    "items_to_include",
    "shortcut_exclude_regex",
    "skip",
];
const UNPUBLISH: &[&str] = &["exclude_regex", "items_to_include", "skip"];

/// Configuration failure before any service mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationError {
    /// Invalid source configuration, retaining ordered diagnostics.
    Invalid(Vec<String>),
    /// Recognized source behavior awaiting its native adapter, never ignored.
    Unsupported(String),
    /// Explicit finite input or traversal bound reached.
    Limit,
    /// Original typed parameter syntax/domain/cancellation/resource failure.
    Transform(Box<crate::parameter_transforms::TransformError>),
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(errors) => {
                write!(f, "Invalid deployment configuration: {}", errors.join("; "))
            }
            Self::Unsupported(branch) => write!(f, "Unported deployment branch: {branch}"),
            Self::Limit => f.write_str("Deployment configuration exceeded its bound"),
            Self::Transform(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConfigurationError {}

/// A source optional setting was skipped because its environment is absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentWarning {
    /// Exact source field name, including its section.
    pub field: String,
    /// Requested environment, retaining case and whitespace.
    pub environment: String,
    /// Available environment keys in source mapping order.
    pub available: Vec<String>,
}

/// A validated configuration with exact selected source environment values.
#[derive(Clone, Debug)]
pub struct DeploymentConfig {
    /// Validated configuration after source overrides, before filesystem resolution.
    pub document: Value,
    /// Source-selected workspace identifier or display name and local input paths.
    pub workspace: Map<String, Value>,
    /// Selected publish fields; empty when the section is absent.
    pub publish: Map<String, Value>,
    /// Selected unpublish fields; absent skip still means false, not implicit consent.
    pub unpublish: Map<String, Value>,
    /// Requested item types, defaulting to all source-supported types.
    pub item_types: Vec<String>,
    /// Exact environment input, not a case-insensitive lookup.
    pub environment: String,
    /// Source warning outcomes; the caller owns presentation and writer failures.
    pub warnings: Vec<EnvironmentWarning>,
    parameters: Option<crate::parameter_transforms::ParameterProgram>,
}

fn invalid(message: impl Into<String>) -> ConfigurationError {
    ConfigurationError::Invalid(vec![message.into()])
}

/// Validate the same UUID spelling accepted by the pinned config validator.
pub(crate) fn valid_guid(value: &str) -> bool {
    let value = value.strip_suffix('\n').unwrap_or(value);
    value.len() == 36
        && value.bytes().enumerate().all(|(index, ch)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                ch == b'-'
            } else {
                ch.is_ascii_hexdigit()
            }
        })
}

fn section_fields(section: &str) -> Option<&'static [&'static str]> {
    match section {
        "core" => Some(CORE),
        "publish" => Some(PUBLISH),
        "unpublish" => Some(UNPUBLISH),
        _ => None,
    }
}

fn apply_overrides(
    document: &mut Map<String, Value>,
    overrides: Option<&Value>,
    environment: &str,
) -> Result<(), ConfigurationError> {
    let Some(overrides) = overrides.filter(|value| override_present(value)) else {
        return Ok(());
    };
    let overrides = overrides
        .as_object()
        .ok_or_else(|| invalid("config_override must be a dictionary"))?;
    let mut errors = Vec::new();
    for (section, value) in overrides {
        let fields = section_fields(section);
        if fields.is_none() && !matches!(section.as_str(), "features" | "constants") {
            errors.push(format!(
                "Cannot override unsupported config section: '{section}'"
            ));
            continue;
        }
        let valid_type = value.is_object() || (section == "features" && value.is_array());
        if !valid_type {
            errors.push(format!("Override section '{section}' has an invalid type"));
            continue;
        }
        if matches!(section.as_str(), "features" | "constants") {
            document.insert(section.clone(), value.clone());
            continue;
        }
        let values = value.as_object().expect("checked object override");
        if let Some(setting) = values
            .keys()
            .find(|setting| !fields.expect("known section").contains(&setting.as_str()))
        {
            errors.push(format!(
                "Cannot override unsupported setting '{section}.{setting}'"
            ));
            continue;
        }
        if !document.contains_key(section) {
            if section == "core" {
                errors.push("Cannot create 'core' section - required section must exist in the config file to override".into());
                continue;
            }
            document.insert(section.clone(), Value::Object(Map::new()));
        }
        let Some(target) = document.get_mut(section).and_then(Value::as_object_mut) else {
            errors.push(format!(
                "Override section '{section}' requires a dictionary in the configuration"
            ));
            continue;
        };
        for (setting, value) in values {
            if section == "core" && !target.contains_key(setting) {
                if setting == "repository_directory" {
                    errors.push("Cannot create required field 'core.repository_directory'".into());
                    continue;
                }
                if matches!(setting.as_str(), "workspace_id" | "workspace") {
                    let other = if setting == "workspace" {
                        "workspace_id"
                    } else {
                        "workspace"
                    };
                    if !target.contains_key(other) {
                        errors.push(format!(
                            "Cannot create workspace identifier 'core.{setting}'"
                        ));
                        continue;
                    }
                }
            }
            if let Some(env_value) = value
                .as_object()
                .and_then(|mapping| mapping.get(environment))
            {
                if let Some(mapping) = target.get_mut(setting).and_then(Value::as_object_mut) {
                    mapping.insert(environment.into(), env_value.clone());
                } else {
                    target.insert(
                        setting.clone(),
                        Value::Object(Map::from_iter([(environment.into(), env_value.clone())])),
                    );
                }
            } else {
                target.insert(setting.clone(), value.clone());
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigurationError::Invalid(errors))
    }
}

fn override_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn check_values(value: &Value, name: &str, accepted: fn(&Value) -> bool, errors: &mut Vec<String>) {
    if let Some(mapping) = value.as_object() {
        if mapping.is_empty() {
            errors.push(format!("'{name}' environment mapping cannot be empty"));
        }
        for (environment, value) in mapping {
            if environment.trim().is_empty() {
                errors.push(format!("'{name}' has an invalid environment key"));
            }
            if !accepted(value) {
                errors.push(format!(
                    "'{name}' value for environment '{environment}' has an invalid type or value"
                ));
            }
        }
    } else if !accepted(value) {
        errors.push(format!("'{name}' has an invalid type or value"));
    }
}

fn nonempty_string(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.trim().is_empty())
}

fn strings(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|list| !list.is_empty() && list.iter().all(nonempty_string))
}

fn selected(section: &Value, key: &str, environment: &str) -> Option<Value> {
    get_config_value(section, key, environment)
        .expect("validated section object")
        .cloned()
}

fn operation(
    document: &Map<String, Value>,
    name: &str,
    environment: &str,
    errors: &mut Vec<String>,
    budget: &mut crate::parameter_transforms::TransformBudget<'_>,
) -> Result<Map<String, Value>, ConfigurationError> {
    let Some(section) = document.get(name) else {
        return Ok(Map::new());
    };
    if !section.is_object() {
        errors.push(format!("'{name}' section must be a dictionary"));
        return Ok(Map::new());
    }
    if name == "unpublish" {
        for field in ["folder_exclude_regex", "folder_path_to_include"] {
            if section.get(field).is_some() {
                errors.push(format!(
                    "'{field}' field is not supported in 'unpublish' section"
                ));
            }
        }
        if section.get("shortcut_exclude_regex").is_some() {
            errors.push("'unpublish.shortcut_exclude_regex' is not supported - shortcut exclusion is only available in the 'publish' section".into());
        }
    }
    let mut result = Map::new();
    for key in section_fields(name).expect("operation section") {
        if let Some(value) = section.get(key) {
            let accepts: fn(&Value) -> bool = match *key {
                "skip" => Value::is_boolean,
                "items_to_include" | "folder_path_to_include" => strings,
                _ => nonempty_string,
            };
            check_values(value, &format!("{name}.{key}"), accepts, errors);
            let values = value
                .as_object()
                .map_or_else(|| vec![value], |mapping| mapping.values().collect());
            for value in values {
                if key.ends_with("regex") {
                    if let Some(pattern) = value.as_str() {
                        if let Err(error) =
                            crate::parameter_transforms::SourceRegex::compile(pattern, budget)
                        {
                            match error {
                                crate::parameter_transforms::TransformError::Invalid(message) => {
                                    errors.push(format!("'{name}.{key}': {message}"))
                                }
                                error => {
                                    return Err(ConfigurationError::Transform(Box::new(error)));
                                }
                            }
                        }
                    }
                }
                if *key == "folder_path_to_include"
                    && value.as_array().is_some_and(|paths| {
                        paths
                            .iter()
                            .any(|path| path.as_str().is_some_and(|path| !path.starts_with('/')))
                    })
                {
                    errors.push(format!("'{name}.{key}' paths must start with '/'"));
                }
            }
            if let Some(value) = selected(section, key, environment) {
                result.insert((*key).into(), value);
            }
        }
    }
    if !section.get("skip").is_some_and(Value::is_boolean)
        && result.contains_key("folder_exclude_regex")
        && result.contains_key("folder_path_to_include")
    {
        errors.push(format!("'{name}.folder_exclude_regex' and '{name}.folder_path_to_include' are mutually exclusive"));
    }
    result.entry("skip").or_insert(Value::Bool(false));
    Ok(result)
}

impl DeploymentConfig {
    /// Apply source overrides, validate all environment values and select settings.
    ///
    /// This API receives a decoded YAML document; it does not introduce a JSON
    /// configuration-file alternative. Filesystem resolution is caller-owned.
    ///
    /// # Errors
    /// Rejects invalid fields/environments and retains unsupported behavior for
    /// explicit preflight in [`Self::require_notebook_publish`].
    pub fn from_document(
        document: &Value,
        environment: &str,
        overrides: Option<&Value>,
    ) -> Result<Self, ConfigurationError> {
        let mut document = document
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("Configuration must be a dictionary"))?;
        apply_overrides(&mut document, overrides, environment)?;
        let core = document
            .get("core")
            .filter(|value| value.is_object())
            .ok_or_else(|| invalid("Configuration must contain a 'core' dictionary"))?;
        let mut errors = Vec::new();
        if core.get("workspace_id").is_none() && core.get("workspace").is_none() {
            errors.push(
                "Configuration must specify either 'workspace_id' or 'workspace' in core section"
                    .into(),
            );
        }
        for key in [
            "workspace_id",
            "workspace",
            "repository_directory",
            "parameter",
        ] {
            if let Some(value) = core.get(key) {
                check_values(value, &format!("core.{key}"), nonempty_string, &mut errors);
                if key == "workspace_id" {
                    check_values(
                        value,
                        "core.workspace_id",
                        |value| value.as_str().is_some_and(valid_guid),
                        &mut errors,
                    );
                }
            } else if key == "repository_directory" {
                errors.push(
                    "Configuration must specify 'repository_directory' in core section".into(),
                );
            }
        }
        if let Some(value) = core.get("item_types_in_scope") {
            check_values(
                value,
                "core.item_types_in_scope",
                |value| {
                    strings(value)
                        && value
                            .as_array()
                            .expect("list")
                            .iter()
                            .all(|value| ITEM_TYPES.contains(&value.as_str().expect("string")))
                },
                &mut errors,
            );
        }
        let mut budget = crate::parameter_transforms::TransformBudget::new(
            crate::parameter_transforms::TransformLimits::default(),
            &|| false,
        );
        let publish = operation(&document, "publish", environment, &mut errors, &mut budget)?;
        let unpublish = operation(
            &document,
            "unpublish",
            environment,
            &mut errors,
            &mut budget,
        )?;
        model::validate_sections(&document, environment, &mut errors);
        for (section_name, keys) in [
            ("core", CORE),
            ("publish", PUBLISH),
            ("unpublish", UNPUBLISH),
        ] {
            if let Some(section) = document.get(section_name).and_then(Value::as_object) {
                for key in keys {
                    if let Some(mapping) = section.get(*key).and_then(Value::as_object) {
                        if environment == "N/A" {
                            errors.push(
                                "Target environment is required with environment mappings".into(),
                            );
                        } else if section_name == "core"
                            && matches!(*key, "workspace_id" | "workspace" | "repository_directory")
                            && !mapping.contains_key(environment)
                        {
                            errors.push(format!(
                                "Environment '{environment}' not found in '{section_name}.{key}'"
                            ));
                        }
                    }
                }
            }
        }
        if !errors.is_empty() {
            return Err(ConfigurationError::Invalid(errors));
        }
        let document = Value::Object(document);
        let workspace = extract_workspace_settings(&document, environment)
            .map_err(|error| invalid(error.to_string()))?;
        let item_types = workspace
            .get("item_types_in_scope")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().expect("validated item type").to_owned())
                    .collect()
            })
            .unwrap_or_else(|| ITEM_TYPES.iter().map(|value| (*value).to_owned()).collect());
        let mut warnings: Vec<_> = ["item_types_in_scope", "parameter"]
            .into_iter()
            .filter_map(|field| {
                document["core"]
                    .get(field)
                    .and_then(Value::as_object)
                    .filter(|mapping| !mapping.contains_key(environment))
                    .map(|mapping| EnvironmentWarning {
                        field: format!("core.{field}"),
                        environment: environment.into(),
                        available: mapping.keys().cloned().collect(),
                    })
            })
            .collect();
        if let Some(mapping) = document.get("features").and_then(Value::as_object)
            && !mapping.contains_key(environment)
        {
            warnings.push(EnvironmentWarning {
                field: "features".into(),
                environment: environment.into(),
                available: mapping.keys().cloned().collect(),
            });
        }
        if environment != "N/A"
            && let Some(constants) = document.get("constants").and_then(Value::as_object)
        {
            for (key, value) in constants {
                if let Some(mapping) = value.as_object()
                    && !mapping.contains_key(environment)
                {
                    warnings.push(EnvironmentWarning {
                        field: format!("constants.{key}"),
                        environment: environment.into(),
                        available: mapping.keys().cloned().collect(),
                    });
                }
            }
        }
        Ok(Self {
            document,
            workspace,
            publish,
            unpublish,
            item_types,
            environment: environment.into(),
            warnings,
            parameters: None,
        })
    }

    /// Fail before mutations for every source branch outside the current publisher.
    ///
    /// Explicit source `unpublish.skip: true` is required: omission defaults to
    /// unpublication upstream and must never be treated as publish-only success.
    ///
    /// # Errors
    /// Returns the precise unsupported field/type, not whole-command rejection.
    pub fn require_notebook_publish(&self) -> Result<(), ConfigurationError> {
        let unsupported = |branch: &str| Err(ConfigurationError::Unsupported(branch.into()));
        if self.workspace.contains_key("workspace_name") {
            return unsupported("core.workspace display-name resolution");
        }
        if self.workspace.contains_key("parameter_file_path") && self.parameters.is_none() {
            return unsupported("core.parameter schema and transforms");
        }
        if self.unpublish.get("skip").and_then(Value::as_bool) != Some(true) {
            return unsupported(
                "unpublish (set unpublish.skip explicitly to true for publish-only)",
            );
        }
        for (name, section) in [("publish", &self.publish), ("unpublish", &self.unpublish)] {
            if let Some(key) = section.keys().find(|key| key.as_str() != "skip") {
                return unsupported(&format!("{name}.{key}"));
            }
        }
        if self
            .item_types
            .iter()
            .any(|item_type| item_type != "Notebook")
        {
            return unsupported("item-specific publishers other than Notebook");
        }
        for key in ["features", "constants"] {
            if self.document.get(key).is_some() {
                return unsupported(key);
            }
        }
        Ok(())
    }

    /// Whether publication is explicitly skipped for the selected environment.
    pub fn skip_publish(&self) -> bool {
        self.publish
            .get("skip")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Bind an explicitly read and decoded parameter file to this configuration.
    ///
    /// Direct consumers must resolve configured parameters before planning;
    /// an unresolved configured file never becomes success-shaped no-op input.
    ///
    /// # Errors
    /// Rejects absent configured paths and invalid/unported parameter schemas.
    pub fn resolve_parameters(&mut self, document: &Value) -> Result<(), ConfigurationError> {
        self.resolve_parameters_controlled(
            document,
            &mut crate::parameter_transforms::TransformBudget::new(
                crate::parameter_transforms::TransformLimits::default(),
                &|| false,
            ),
        )
    }

    /// Resolve using a caller-owned finite syntax/work/deadline/cancellation budget.
    ///
    /// # Errors
    /// Retains original transform errors and clears stale prior parameters even
    /// when a second resolution is cancelled, invalid or outside the supported set.
    pub fn resolve_parameters_controlled(
        &mut self,
        document: &Value,
        budget: &mut crate::parameter_transforms::TransformBudget<'_>,
    ) -> Result<(), ConfigurationError> {
        self.parameters = None;
        if !self.workspace.contains_key("parameter_file_path") {
            return Err(invalid(
                "No parameter file was configured for this environment",
            ));
        }
        self.parameters = Some(
            crate::parameter_transforms::ParameterProgram::from_document_with_features(
                document,
                &self.environment,
                self.command_configuration(CommandOptions::default())?
                    .feature_enabled("enable_environment_variable_replacement"),
                budget,
            )
            .map_err(|error| ConfigurationError::Transform(Box::new(error)))?,
        );
        Ok(())
    }

    /// Explicitly validated source parameters, never an ambient auto-discovery.
    pub fn parameters(&self) -> Option<&crate::parameter_transforms::ParameterProgram> {
        self.parameters.as_ref()
    }

    /// Full prepared parameter source after explicit successful resolution.
    ///
    /// No source is auto-discovered or read here. None also follows a failed
    /// resolution, so a stale previously validated document cannot be reused.
    pub fn raw_parameter_document(&self) -> Option<&Value> {
        self.parameters()
            .map(crate::parameter_transforms::ParameterProgram::source_document)
    }

    /// Select configured constant overrides for the exact target environment.
    ///
    /// Every dictionary is an environment mapping, including runtime mappings
    /// such as API_FORMAT_MAPPING: those require an environment-wrapped value.
    /// Missing environments and null values are omitted; false, zero and empty
    /// strings remain values. The returned snapshot neither recomputes aliases
    /// nor changes caller globals or this validated source document.
    pub fn selected_constants(&self) -> Map<String, Value> {
        model::selected_constants(self)
    }

    /// Resolve templates and parameter features through caller-owned capabilities.
    ///
    /// The host decodes each template with parameter duplicate-key policy after
    /// injected environment-token expansion. This method never reads files or
    /// environment variables and returns skipped-template outcomes to the caller.
    ///
    /// # Errors
    /// Clears any prior program, then preserves host, syntax, schema and resource
    /// errors. A configured parameter path is required; absence never auto-discovers.
    pub fn resolve_parameters_with_host(
        &mut self,
        document: &Value,
        host: &dyn crate::parameter_transforms::ParameterFileHost,
        options: CommandOptions,
        budget: &mut crate::parameter_transforms::TransformBudget<'_>,
    ) -> Result<crate::parameter_transforms::PreparedParameters, ConfigurationError> {
        self.parameters = None;
        if !self.workspace.contains_key("parameter_file_path") {
            return Err(invalid(
                "No parameter file was configured for this environment",
            ));
        }
        let enabled = self
            .command_configuration(options)?
            .feature_enabled("enable_environment_variable_replacement");
        let prepared =
            crate::parameter_transforms::prepare_parameter_document(document, host, budget)
                .map_err(|error| ConfigurationError::Transform(Box::new(error)))?;
        self.parameters = Some(
            crate::parameter_transforms::ParameterProgram::from_document_with_features(
                &prepared.document,
                &self.environment,
                enabled,
                budget,
            )
            .map_err(|error| ConfigurationError::Transform(Box::new(error)))?,
        );
        Ok(prepared)
    }

    /// Freeze selected configuration and caller-owned global options for one command.
    ///
    /// Unlike the legacy Notebook preflight, this retains every source setting
    /// for the orchestrator and never installs process-global flags or constants.
    /// Borrowed options are supported without removing the earlier owned call
    /// form. The snapshot owns its immutable copy of noncredential metadata.
    ///
    /// # Errors
    /// Rejects inconsistent caller options; extension fields are retained and
    /// consumers must explicitly acknowledge them before executing mutations.
    pub fn command_configuration(
        &self,
        options: impl std::borrow::Borrow<CommandOptions>,
    ) -> Result<CommandConfiguration, ConfigurationError> {
        let options: &CommandOptions = options.borrow();
        model::snapshot(self, options.clone())
    }
}
