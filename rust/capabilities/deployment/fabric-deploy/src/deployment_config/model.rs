// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ConfigurationError, DeploymentConfig, invalid, section_fields};
use serde_json::{Map, Value};
use std::{ops::Deref, sync::Arc};

/// Source workspace selection, preserving ID precedence over a display name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceTarget {
    /// Exact configured ID spelling; service validation belongs to the consumer.
    Id(String),
    /// Exact display name, resolved through caller-owned workspace discovery.
    Name(String),
}

/// Caller-owned inputs replacing mutable source globals for one deployment.
#[derive(Clone, Debug, Default)]
pub struct CommandOptions {
    /// CLI bulk option adds both experimental and bulk flags before config flags.
    pub bulk_publish: bool,
    /// Existing caller flags, retained exactly rather than discovered globally.
    pub features: Vec<String>,
    /// Existing caller constant values, overridden by selected config constants.
    pub constants: Map<String, Value>,
    /// Noncredential keyword metadata; only `host_app` is consumed upstream.
    ///
    /// The CLI wrapper overwrites `host_app`. Credentials/SDK identity are
    /// injected separately and must never be supplied through this map.
    pub kwargs: Map<String, Value>,
}

impl CommandOptions {
    /// Construct command inputs from bulk selection and noncredential metadata.
    ///
    /// Optional source-global snapshots remain empty unless explicitly injected
    /// through the existing `features` and `constants` compatibility fields.
    pub fn new(bulk_publish: bool, kwargs: Map<String, Value>) -> Self {
        Self {
            bulk_publish,
            kwargs,
            ..Self::default()
        }
    }
}

/// Selected publication or unpublication settings, without implicit execution.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OperationSettings {
    /// Source skip value; omitted settings default to false.
    pub skip: bool,
    /// Item-name exclusion regex.
    pub exclude_regex: Option<String>,
    /// Folder exclusion regex, publish only.
    pub folder_exclude_regex: Option<String>,
    /// Exact folder inclusion paths, publish only.
    pub folder_path_to_include: Option<Vec<String>>,
    /// Exact source item selection values.
    pub items_to_include: Option<Vec<String>>,
    /// Shortcut-name exclusion regex, publish only.
    pub shortcut_exclude_regex: Option<String>,
}

impl OperationSettings {
    fn from_mapping(mapping: &Map<String, Value>) -> Self {
        let text = |key| mapping.get(key).and_then(Value::as_str).map(str::to_owned);
        let list = |key| {
            mapping.get(key).and_then(Value::as_array).map(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().expect("validated list").to_owned())
                    .collect()
            })
        };
        Self {
            skip: mapping
                .get("skip")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            exclude_regex: text("exclude_regex"),
            folder_exclude_regex: text("folder_exclude_regex"),
            folder_path_to_include: list("folder_path_to_include"),
            items_to_include: list("items_to_include"),
            shortcut_exclude_regex: text("shortcut_exclude_regex"),
        }
    }

    /// Whether this operation is skipped; absence defaults to false upstream.
    pub fn skip(&self) -> bool {
        self.skip
    }
    /// Item-name exclusion regex, or no configured exclusion.
    pub fn exclude_regex(&self) -> Option<&str> {
        self.exclude_regex.as_deref()
    }
    /// Source orphan-unpublish default when no exclusion was selected.
    pub fn orphan_exclude_regex(&self) -> &str {
        self.exclude_regex.as_deref().unwrap_or("^$")
    }
    /// Folder exclusion regex, publish only.
    pub fn folder_exclude_regex(&self) -> Option<&str> {
        self.folder_exclude_regex.as_deref()
    }
    /// Exact configured absolute folder paths, publish only.
    pub fn folder_path_to_include(&self) -> Option<&[String]> {
        self.folder_path_to_include.as_deref()
    }
    /// Exact source item selection values.
    pub fn items_to_include(&self) -> Option<&[String]> {
        self.items_to_include.as_deref()
    }
    /// Shortcut-name exclusion regex, publish only.
    pub fn shortcut_exclude_regex(&self) -> Option<&str> {
        self.shortcut_exclude_regex.as_deref()
    }
}

/// Immutable, command-scoped source configuration handed to orchestration.
///
/// It contains no credentials, environment reads, filesystem access or globals.
/// Unknown source fields remain lossless extensions, not silently dropped input.
#[derive(Clone, Debug)]
pub struct CommandConfiguration {
    fields: Arc<CommandConfigurationData>,
}

/// Public read-only field view of a validated immutable command snapshot.
///
/// [`CommandConfiguration`] exposes these fields through [`Deref`], without
/// `DerefMut`; callers can read/clone them but cannot mutate the frozen snapshot.
#[derive(Clone, Debug)]
pub struct CommandConfigurationData {
    /// Complete merged source document, including all unselected environments.
    pub raw_document: Value,
    /// Source ID/name selector with workspace-ID precedence.
    pub workspace: WorkspaceTarget,
    /// Exact selected target environment.
    pub environment: String,
    /// Source-selected path; host owns resolution and filesystem validation.
    pub repository_directory: String,
    /// Explicit parameter path; None forbids implicit discovery.
    pub parameter_file_path: Option<String>,
    /// Selected source item types, without a Notebook-only restriction.
    pub item_types: Vec<String>,
    /// Every selected source publish setting.
    pub publish: OperationSettings,
    /// Every selected source unpublish setting, including skip=false by default.
    pub unpublish: OperationSettings,
    /// Exact-case base/config feature entries before a FEATURE_FLAG override.
    pub features: Vec<String>,
    /// Caller constants overlaid with exact source-selected configuration values.
    pub constants: Map<String, Value>,
    /// Noncredential source keyword metadata, including source-ignored arguments.
    pub kwargs: Map<String, Value>,
    /// Unknown source properties retained for explicit emission handling.
    pub extensions: Map<String, Value>,
    parameters: Option<crate::parameter_transforms::ParameterProgram>,
}

impl Deref for CommandConfiguration {
    type Target = CommandConfigurationData;

    fn deref(&self) -> &Self::Target {
        &self.fields
    }
}

impl CommandConfiguration {
    /// Full merged source document, retaining unselected environments and fields.
    pub fn document(&self) -> &Value {
        &self.raw_document
    }
    /// Full merged source document using the engine field-view naming.
    pub fn raw_document(&self) -> &Value {
        &self.raw_document
    }
    /// Workspace ID or display name with source precedence.
    pub fn target(&self) -> &WorkspaceTarget {
        &self.workspace
    }
    /// Workspace selector using the engine field-view naming.
    pub fn workspace(&self) -> &WorkspaceTarget {
        &self.workspace
    }
    /// Exact deployment environment.
    pub fn environment(&self) -> &str {
        &self.environment
    }
    /// Source-selected unresolved path, resolved by an injected host.
    pub fn repository_directory(&self) -> &str {
        &self.repository_directory
    }
    /// Explicit parameter path; None forbids implicit repository auto-discovery.
    pub fn parameter_file(&self) -> Option<&str> {
        self.parameter_file_path.as_deref()
    }
    /// Explicit configured parameter path using the engine field-view naming.
    pub fn parameter_file_path(&self) -> Option<&str> {
        self.parameter_file_path.as_deref()
    }
    /// All selected source-supported types, without a Notebook-only restriction.
    pub fn item_types(&self) -> &[String] {
        &self.item_types
    }
    /// Selected publish operation settings.
    pub fn publish(&self) -> &OperationSettings {
        &self.publish
    }
    /// Selected unpublish settings, including the default skip=false.
    pub fn unpublish(&self) -> &OperationSettings {
        &self.unpublish
    }
    /// Exact-case feature set, in first-enabled order.
    pub fn features(&self) -> &[String] {
        &self.features
    }
    /// Test a source feature name exactly, without truthiness or case folding.
    pub fn feature_enabled(&self, feature: &str) -> bool {
        if let Some(flags) = self.constants.get("FEATURE_FLAG") {
            return membership(flags, feature).expect("validated iterable flag constant");
        }
        self.features.iter().any(|value| value == feature)
    }
    /// Evaluate an injected environment flag with source VALID_ENABLE_FLAGS.
    ///
    /// This is distinct from parameter string flags: defaults accept `"1"`,
    /// `"true"` and `"yes"` after lowercasing, with no whitespace stripping.
    ///
    /// # Errors
    /// A configured non-iterable VALID_ENABLE_FLAGS fails rather than becoming false.
    pub fn environment_flag_enabled(&self, value: &str) -> Result<bool, ConfigurationError> {
        let value = value.to_lowercase();
        self.constants.get("VALID_ENABLE_FLAGS").map_or_else(
            || Ok(["1", "true", "yes"].contains(&value.as_str())),
            |flags| membership(flags, &value),
        )
    }
    /// Typed constants after exact per-environment overrides.
    pub fn constants(&self) -> &Map<String, Value> {
        &self.constants
    }
    /// Lossless keyword arguments, including unknown source-ignored arguments.
    pub fn kwargs(&self) -> &Map<String, Value> {
        &self.kwargs
    }
    /// Source-selected host application keyword, without coercion.
    pub fn host_app(&self) -> Option<&Value> {
        self.kwargs.get("host_app")
    }
    /// Unknown fields indexed by source field path, retaining their typed values.
    pub fn extensions(&self) -> &Map<String, Value> {
        &self.extensions
    }
    /// The resolved command-local parameter program, if explicitly configured.
    pub fn parameters(&self) -> Option<&crate::parameter_transforms::ParameterProgram> {
        self.parameters.as_ref()
    }

    /// Validate source operation feature dependencies without changing globals.
    ///
    /// Parsing accepts configured settings before these publication-time checks.
    /// Disabled folder publication preserves the settings but skips their feature
    /// gates exactly as source does. Item scope/bulk fallback remains caller-owned.
    ///
    /// # Errors
    /// Rejects missing feature dependencies and conflicting active folder filters.
    pub fn validate_operation_features(&self) -> Result<(), ConfigurationError> {
        let require = |feature| {
            if self.feature_enabled("enable_experimental_features") && self.feature_enabled(feature)
            {
                Ok(())
            } else {
                Err(invalid(format!(
                    "Configured operation requires enable_experimental_features and {feature}"
                )))
            }
        };
        if !self.publish.skip() {
            if self.feature_enabled("enable_bulk_publish")
                && !self.feature_enabled("enable_experimental_features")
            {
                return Err(invalid(
                    "enable_bulk_publish requires enable_experimental_features",
                ));
            }
            if self.publish.items_to_include().is_some() {
                require("enable_items_to_include")?;
            }
            if self.publish.shortcut_exclude_regex().is_some() {
                require("enable_shortcut_exclude")?;
            }
            if !self.feature_enabled("disable_workspace_folder_publish") {
                if self.publish.folder_exclude_regex().is_some()
                    && self.publish.folder_path_to_include().is_some()
                {
                    return Err(invalid(
                        "Folder exclusion and inclusion cannot both be active",
                    ));
                }
                if self.publish.folder_exclude_regex().is_some() {
                    require("enable_exclude_folder")?;
                }
                if self.publish.folder_path_to_include().is_some() {
                    require("enable_include_folder")?;
                }
            }
        }
        if !self.unpublish.skip() && self.unpublish.items_to_include().is_some() {
            require("enable_items_to_include")?;
        }
        Ok(())
    }

    /// Refuse extension emission until the consumer explicitly implements it.
    ///
    /// Opt into rejecting source-ignored fields for a stricter embedded caller.
    ///
    /// The pinned deployment wrapper extracts only known settings and ignores
    /// these properties during execution. The source-compatible CLI therefore
    /// retains them in the snapshot without invoking this optional policy.
    ///
    /// # Errors
    /// Returns the first unconsumed property path, before any mutation.
    pub fn require_no_extensions(&self) -> Result<(), ConfigurationError> {
        if let Some(key) = self.extensions.keys().next() {
            return Err(ConfigurationError::Unsupported(format!(
                "configuration extension {key}"
            )));
        }
        Ok(())
    }
}

pub(super) fn selected_constants(config: &DeploymentConfig) -> Map<String, Value> {
    let mut constants = Map::new();
    if let Some(values) = config.document.get("constants").and_then(Value::as_object) {
        for (key, value) in values {
            let selected = if let Some(mapping) = value.as_object() {
                mapping.get(&config.environment)
            } else {
                Some(value)
            };
            if let Some(value) = selected.filter(|value| !value.is_null()) {
                constants.insert(key.clone(), value.clone());
            }
        }
    }
    constants
}

pub(super) fn snapshot(
    config: &DeploymentConfig,
    options: CommandOptions,
) -> Result<CommandConfiguration, ConfigurationError> {
    let target = if let Some(value) = config.workspace.get("workspace_id").and_then(Value::as_str) {
        WorkspaceTarget::Id(value.into())
    } else if let Some(value) = config
        .workspace
        .get("workspace_name")
        .and_then(Value::as_str)
    {
        WorkspaceTarget::Name(value.into())
    } else {
        return Err(invalid("Selected workspace identifier is absent"));
    };
    let mut features = Vec::new();
    let mut add = |feature: String| {
        if !features.contains(&feature) {
            features.push(feature);
        }
    };
    for feature in options.features {
        add(feature);
    }
    if options.bulk_publish {
        add("enable_experimental_features".into());
        add("enable_bulk_publish".into());
    }
    if let Some(value) = config.document.get("features") {
        let selected = value
            .as_object()
            .and_then(|values| values.get(&config.environment))
            .unwrap_or(value);
        if let Some(values) = selected.as_array() {
            for value in values {
                add(value.as_str().expect("validated feature").into());
            }
        }
    }
    let mut constants = options.constants;
    constants.extend(selected_constants(config));
    if let Some(flags) = constants.get("FEATURE_FLAG") {
        membership(flags, "")?;
    }
    let mut extensions = Map::new();
    for (key, value) in config.document.as_object().expect("validated document") {
        if let Some(fields) = section_fields(key) {
            for (field, value) in value.as_object().expect("validated section") {
                if !fields.contains(&field.as_str()) {
                    extensions.insert(format!("{key}.{field}"), value.clone());
                }
            }
        } else if !["features", "constants"].contains(&key.as_str()) {
            extensions.insert(key.clone(), value.clone());
        }
    }
    Ok(CommandConfiguration {
        fields: Arc::new(CommandConfigurationData {
            raw_document: config.document.clone(),
            workspace: target,
            environment: config.environment.clone(),
            repository_directory: config
                .workspace
                .get("repository_directory")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Selected repository_directory is absent"))?
                .into(),
            parameter_file_path: config
                .workspace
                .get("parameter_file_path")
                .and_then(Value::as_str)
                .map(str::to_owned),
            item_types: config.item_types.clone(),
            publish: OperationSettings::from_mapping(&config.publish),
            unpublish: OperationSettings::from_mapping(&config.unpublish),
            features,
            constants,
            kwargs: options.kwargs,
            extensions,
            parameters: config.parameters().cloned(),
        }),
    })
}

fn membership(value: &Value, entry: &str) -> Result<bool, ConfigurationError> {
    match value {
        Value::String(value) => Ok(value.contains(entry)),
        Value::Array(values) => Ok(values.iter().any(|value| value.as_str() == Some(entry))),
        Value::Object(values) => Ok(values.contains_key(entry)),
        _ => Err(invalid(
            "Source flag constant must support membership testing",
        )),
    }
}

pub(super) fn validate_sections(
    document: &Map<String, Value>,
    environment: &str,
    errors: &mut Vec<String>,
) {
    if let Some(features) = document.get("features") {
        super::check_values(features, "features", super::strings, errors);
        if environment == "N/A" && features.is_object() {
            errors.push("Target environment is required with environment mappings".into());
        }
    }
    if let Some(constants) = document.get("constants") {
        let Some(constants) = constants.as_object() else {
            errors.push("'constants' section must be a dictionary".into());
            return;
        };
        for (key, value) in constants {
            if !CONSTANT_NAMES.contains(&key.as_str()) {
                errors.push(format!("Unknown source constant '{key}'"));
            }
            let values = if let Some(values) = value.as_object() {
                for environment in values.keys() {
                    if environment.trim().is_empty() {
                        errors.push(format!("'constants.{key}' has an invalid environment key"));
                    }
                }
                values.values().collect::<Vec<_>>()
            } else {
                vec![value]
            };
            if ["DEFAULT_API_ROOT_URL", "FABRIC_API_ROOT_URL"].contains(&key.as_str()) {
                for value in values {
                    if !value.as_str().is_some_and(valid_api_url) {
                        errors.push(format!(
                            "'constants.{key}' must be an HTTPS source API root URL"
                        ));
                    }
                }
            }
        }
    }
}

fn valid_api_url(value: &str) -> bool {
    let value = value.trim_start_matches(|character: char| character <= ' ');
    let value = value.replace(['\t', '\r', '\n'], "");
    let Some((scheme, remainder)) = value.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    let host = host.to_lowercase();
    let valid_suffix = host
        .strip_suffix("api.fabric.microsoft.com")
        .or_else(|| host.strip_suffix("api.powerbi.com"));
    let Some(prefix) = valid_suffix else {
        return false;
    };
    if !prefix
        .chars()
        .all(|character| character.is_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return false;
    }
    let labels: Vec<_> = prefix.split('.').collect();
    if labels[..labels.len() - 1]
        .iter()
        .any(|label| label.is_empty())
    {
        return false;
    }
    let tail = &remainder[authority_end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = &tail[..path_end];
    path.is_empty() || path == "/"
}

// Names bound by the exact pinned constants module, not a new configuration schema.
const CONSTANT_NAMES: &[&str] = &[
    "ACCEPTED_ITEM_TYPES",
    "API_FORMAT_MAPPING",
    "ASYNC_PROVISIONED_ATTRIBUTES",
    "BULK_ACCEPTED_ITEM_TYPES",
    "BULK_ITEM_COUNT_LIMIT",
    "BULK_UNSUPPORTED_ITEM_TYPES",
    "CONFIG_SECTIONS",
    "CONFIG_VALIDATION_MSGS",
    "CROSS_WORKSPACE_ITEM_SEPARATOR",
    "DATAFLOW_SOURCE_REGEX",
    "DATA_PIPELINE_CONTENT_FILE_JSON",
    "DEFAULT_API_ROOT_URL",
    "DEFAULT_GUID",
    "DYNAMIC_VARIABLES_REGEX",
    "DYNAMIC_VARIABLE_MSGS",
    "ENVIRONMENT_VARIABLE_PREFIX",
    "EXCLUDE_PATH_REGEX_MAPPING",
    "Enum",
    "EnvVar",
    "FABRIC_API_ROOT_URL",
    "FEATURE_FLAG",
    "FeatureFlag",
    "INDENT",
    "INVALID_FOLDER_CHAR_REGEX",
    "INVALID_REPLACE_VALUE_SPARK_POOL",
    "ITEM_ATTR_LOOKUP",
    "ITEM_TYPE_TO_FILE",
    "ITEM_VARIABLE_PREFIX",
    "ItemType",
    "KQL_DATABASE_FOLDER_PATH_REGEX",
    "NON_MOVABLE_ITEM_TYPES",
    "NO_ASSIGNED_CAPACITY_REQUIRED",
    "ORG_APP_AUDIENCE_FOLDER_PATH_REGEX",
    "OperationType",
    "PARALLEL_MAX_WORKERS",
    "PARAMETER_FILE_FILTERS",
    "PARAMETER_FILE_NAME",
    "PARAMETER_MSGS",
    "PARAM_NAMES",
    "PROPERTY_PATH_ATTR_MAPPING",
    "REFLEX_WORKSPACE_ID_REFERENCE_REGEX",
    "RETRY_AFTER_SECONDS",
    "RETRY_API_MAX_DURATION_SECONDS",
    "RETRY_BASE_DELAY_SECONDS",
    "RETRY_MAX_DURATION_SECONDS",
    "SENSITIVE_REQUEST_HEADERS",
    "SENSITIVE_RESPONSE_HEADERS",
    "SERIAL_ITEM_PUBLISH_ORDER",
    "SHELL_ONLY_PUBLISH",
    "UNPUBLISH_FLAG_MAPPING",
    "VALID_ENABLE_FLAGS",
    "VALID_GUID_REGEX",
    "VERSION",
    "WILDCARD_PATH_VALIDATIONS",
    "WORKSPACE_ID_REFERENCE_REGEX",
    "WORKSPACE_VARIABLES_FIXED",
    "WORKSPACE_VARIABLE_ATTRIBUTES",
    "WORKSPACE_VARIABLE_PREFIX",
    "YAML_FILE_EXTENSIONS",
    "_parallel_max_workers_raw",
    "_retry_api_max_duration_raw",
    "os",
    "validate_env_var_api_url",
    "__name__",
    "__doc__",
    "__package__",
    "__loader__",
    "__spec__",
    "__file__",
    "__cached__",
    "__builtins__",
    "__annotations__",
];
