// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure source-order planning and selective publication without mutable globals.

use crate::{
    config::get_config_value,
    deployment_config::{ConfigurationError, DeploymentConfig, ITEM_TYPES},
    model::{RepositoryItem, invalid},
    processors::Processor,
    repository::DeploymentRepository,
    transforms::DEFAULT_GUID,
};
use fancy_regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};

/// Explicit invocation-local feature policy, restored by dropping this value.
#[derive(Clone, Debug)]
pub struct EngineContext {
    /// Selected source features; libraries never mutate process-wide flags.
    pub features: HashSet<String>,
    /// Selected source constants; no environment or process-global discovery.
    pub constants: Map<String, Value>,
    /// Requested bulk mode after command-local source flags have been applied.
    pub bulk_publish: bool,
    /// Explicitly read parameter source, including processor-specific settings.
    pub parameter_document: Option<Value>,
    /// Explicit caller snapshot for opt-in `$ENV:` replacement, never ambient reads.
    pub environment_variables: HashMap<String, String>,
    /// Aggregate prepared-definition byte budget, independent of service responses.
    pub definition_limits: crate::notebook::DefinitionLimits,
}

impl EngineContext {
    /// Select configuration features/constants and validate experimental policies.
    ///
    /// # Errors
    /// A gated source option without its required flags is real validation failure.
    pub fn from_config(
        config: &DeploymentConfig,
        command_bulk: bool,
    ) -> Result<Self, ConfigurationError> {
        let features = config
            .document
            .get("features")
            .map(|value| {
                value
                    .as_object()
                    .and_then(|mapping| mapping.get(&config.environment))
                    .unwrap_or(value)
            })
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut context = Self {
            features,
            constants: Map::new(),
            bulk_publish: command_bulk,
            parameter_document: None,
            environment_variables: HashMap::new(),
            definition_limits: crate::notebook::DefinitionLimits::default(),
        };
        if command_bulk {
            context.features.insert("enable_bulk_publish".into());
            context
                .features
                .insert("enable_experimental_features".into());
        }
        if let Some(constants) = config.document.get("constants").and_then(Value::as_object) {
            for key in constants.keys() {
                if let Some(value) =
                    get_config_value(&Value::Object(constants.clone()), key, &config.environment)
                        .map_err(|_| invalid("Invalid constant environment selection"))?
                {
                    if !value.is_null() {
                        context.constants.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        if let Some(flags) = context.constants.get("FEATURE_FLAG") {
            context.features = flags
                .as_array()
                .ok_or_else(|| invalid("FEATURE_FLAG must be a list"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("FEATURE_FLAG entries must be strings"))
                })
                .collect::<Result<_, _>>()?;
        }
        context.bulk_publish = context.enabled("enable_bulk_publish");
        context.validate_constants()?;
        for (section, key, required) in [
            (
                &config.publish,
                "items_to_include",
                "enable_items_to_include",
            ),
            (
                &config.unpublish,
                "items_to_include",
                "enable_items_to_include",
            ),
            (
                &config.publish,
                "folder_exclude_regex",
                "enable_exclude_folder",
            ),
            (
                &config.publish,
                "folder_path_to_include",
                "enable_include_folder",
            ),
            (
                &config.publish,
                "shortcut_exclude_regex",
                "enable_shortcut_exclude",
            ),
        ] {
            if section.get("skip").and_then(Value::as_bool) == Some(true)
                || (key.starts_with("folder_")
                    && context.enabled("disable_workspace_folder_publish"))
            {
                continue;
            }
            if section.contains_key(key)
                && (!context.enabled("enable_experimental_features") || !context.enabled(required))
            {
                return Err(invalid(
                    "Selective deployment requires experimental and option-specific feature flags",
                ));
            }
        }
        if !config.skip_publish()
            && context.bulk_publish
            && !context.enabled("enable_experimental_features")
        {
            return Err(invalid(
                "enable_bulk_publish requires enable_experimental_features",
            ));
        }
        if config.publish.contains_key("folder_exclude_regex")
            && config.publish.contains_key("folder_path_to_include")
            && !context.enabled("disable_workspace_folder_publish")
        {
            return Err(invalid(
                "Folder exclusion and inclusion cannot be used together",
            ));
        }
        Ok(context)
    }

    /// Query one source feature without global state.
    pub fn enabled(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }

    fn validate_constants(&self) -> Result<(), ConfigurationError> {
        for key in [
            "SHELL_ONLY_PUBLISH",
            "NON_MOVABLE_ITEM_TYPES",
            "NO_ASSIGNED_CAPACITY_REQUIRED",
            "BULK_ACCEPTED_ITEM_TYPES",
            "BULK_UNSUPPORTED_ITEM_TYPES",
            "ACCEPTED_ITEM_TYPES",
            "ASYNC_PROVISIONED_ATTRIBUTES",
            "PARAMETER_FILE_FILTERS",
            "YAML_FILE_EXTENSIONS",
            "ITEM_ATTR_LOOKUP",
        ] {
            if self.constants.get(key).is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|values| values.iter().any(|value| !value.is_string()))
            }) {
                return Err(invalid(
                    "Runtime item/attribute policy constants must be lists of strings",
                ));
            }
        }
        for key in [
            "EXCLUDE_PATH_REGEX_MAPPING",
            "API_FORMAT_MAPPING",
            "UNPUBLISH_FLAG_MAPPING",
            "ITEM_TYPE_TO_FILE",
            "PROPERTY_PATH_ATTR_MAPPING",
        ] {
            if self
                .constants
                .get(key)
                .is_some_and(|value| !value.is_object())
            {
                return Err(invalid(
                    "Runtime mapping policy constant must be a dictionary",
                ));
            }
        }
        for key in [
            "DEFAULT_GUID",
            "WORKSPACE_ID_REFERENCE_REGEX",
            "REFLEX_WORKSPACE_ID_REFERENCE_REGEX",
            "DATAFLOW_SOURCE_REGEX",
            "INVALID_FOLDER_CHAR_REGEX",
            "KQL_DATABASE_FOLDER_PATH_REGEX",
            "ORG_APP_AUDIENCE_FOLDER_PATH_REGEX",
            "VALID_GUID_REGEX",
        ] {
            if let Some(value) = self.constants.get(key) {
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid("Runtime reference constant must be a string"))?;
                if key.ends_with("REGEX") {
                    Regex::new(value).map_err(|_| invalid("Invalid runtime reference regex"))?;
                }
            }
        }
        if self
            .constants
            .get("BULK_ITEM_COUNT_LIMIT")
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(invalid(
                "BULK_ITEM_COUNT_LIMIT must be a nonnegative integer",
            ));
        }
        if let Some(order) = self.constants.get("SERIAL_ITEM_PUBLISH_ORDER") {
            if !order.as_object().is_some_and(|order| order.is_empty()) {
                return Err(invalid(
                    "Pinned source publication order requires ItemType enum values, which safe configuration scalars cannot construct",
                ));
            }
        }
        Ok(())
    }

    /// Selected scalar constant, after source environment selection.
    pub fn text_constant<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.constants
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
    }

    /// Membership in a selected source list constant.
    pub fn listed(&self, key: &str, defaults: &[&str], value: &str) -> bool {
        self.constants
            .get(key)
            .and_then(Value::as_array)
            .map_or_else(
                || defaults.contains(&value),
                |values| values.iter().any(|entry| entry.as_str() == Some(value)),
            )
    }

    /// Effective source placeholder, which need not be a service item ID.
    pub fn default_guid(&self) -> &str {
        self.text_constant("DEFAULT_GUID", DEFAULT_GUID)
    }

    /// Source shell-only policy, including configured replacements.
    pub fn shell_only(&self, item_type: &str) -> bool {
        self.listed(
            "SHELL_ONLY_PUBLISH",
            &["Lakehouse", "Warehouse", "SQLDatabase", "MLExperiment"],
            item_type,
        )
    }

    /// Whether the source bulk API admits every requested type.
    pub fn bulk_admits(&self, types: &[String]) -> bool {
        types.iter().all(|item_type| {
            self.constants
                .get("BULK_ACCEPTED_ITEM_TYPES")
                .and_then(Value::as_array)
                .map_or_else(
                    || !["Warehouse", "DataBuildToolJob"].contains(&item_type.as_str()),
                    |values| values.iter().any(|value| value.as_str() == Some(item_type)),
                )
        })
    }

    /// Source type order; an explicitly empty order schedules no types.
    pub fn type_order(&self) -> &[&str] {
        if self.constants.contains_key("SERIAL_ITEM_PUBLISH_ORDER") {
            &[]
        } else {
            ITEM_TYPES
        }
    }

    /// Definition file used by the source dependency publisher.
    ///
    /// # Errors
    /// Missing mapping entries and paths outside the item capability are invalid.
    pub fn dependency_file(&self, item_type: &str) -> Result<String, ConfigurationError> {
        let path = if let Some(mapping) = self
            .constants
            .get("ITEM_TYPE_TO_FILE")
            .and_then(Value::as_object)
        {
            mapping
                .get(item_type)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Missing dependency file mapping"))?
        } else {
            "pipeline-content.json"
        };
        let path = path.replace('\\', "/");
        if path.is_empty()
            || path.contains([':', '\0'])
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(invalid(
                "Dependency file mapping escapes the item capability",
            ));
        }
        Ok(path)
    }

    /// Source metadata path for a dynamically resolved/provisioned item attribute.
    pub fn attribute_pointer(&self, item_type: &str, attribute: &str) -> Option<String> {
        let path = if let Some(mapping) = self
            .constants
            .get("PROPERTY_PATH_ATTR_MAPPING")
            .and_then(Value::as_object)
        {
            mapping.get(item_type)?.get(attribute)?.as_str()?
        } else {
            match (item_type, attribute) {
                ("Lakehouse" | "MirroredDatabase", "sqlendpoint") => {
                    "body/properties/sqlEndpointProperties/connectionString"
                }
                ("Lakehouse" | "MirroredDatabase", "sqlendpointid") => {
                    "body/properties/sqlEndpointProperties/id"
                }
                ("Warehouse", "sqlendpoint") => "body/properties/connectionString",
                ("SQLDatabase", "sqlendpoint" | "sqlendpointfqdn") => "body/properties/serverFqdn",
                ("Eventhouse", "queryserviceuri") => "body/properties/queryServiceUri",
                _ => return None,
            }
        };
        Some(format!("/{}", path.strip_prefix("body/").unwrap_or(path)))
    }

    /// Attach explicit parameter source without rediscovering files or environment.
    pub fn with_parameter_document(mut self, document: Value) -> Self {
        self.parameter_document = Some(document);
        self
    }

    /// Apply caller-owned local definition bounds without changing source policy.
    pub fn with_definition_limits(mut self, limits: crate::notebook::DefinitionLimits) -> Self {
        self.definition_limits = limits;
        self
    }

    /// Source unpublication feature gates protect provisioned/data-bearing items.
    pub fn can_unpublish(&self, item_type: &str) -> bool {
        if let Some(mapping) = self
            .constants
            .get("UNPUBLISH_FLAG_MAPPING")
            .and_then(Value::as_object)
        {
            return mapping
                .get(item_type)
                .and_then(Value::as_str)
                .is_none_or(|feature| self.enabled(feature));
        }
        let flag = match item_type {
            "Lakehouse" => "enable_lakehouse_unpublish",
            "Warehouse" => "enable_warehouse_unpublish",
            "SQLDatabase" => "enable_sqldatabase_unpublish",
            "Eventhouse" => "enable_eventhouse_unpublish",
            "KQLDatabase" => "enable_kqldatabase_unpublish",
            "CosmosDBDatabase" => "enable_cosmosdbdatabase_unpublish",
            "GraphModel" => "enable_graphmodel_unpublish",
            _ => return true,
        };
        self.enabled(flag)
    }
}

/// Precompiled source filters; arbitrary regex text is never a service URL.
pub struct PublishSelection {
    name: Option<Regex>,
    folder: Option<Regex>,
    folders: Option<Vec<String>>,
    items: Option<HashSet<String>>,
    folders_disabled: bool,
}

fn pattern(value: Option<&Value>) -> Result<Option<Regex>, ConfigurationError> {
    value
        .and_then(Value::as_str)
        .map(|pattern| {
            Regex::new(pattern).map_err(|_| invalid("Invalid deployment exclusion regex"))
        })
        .transpose()
}

fn regex_matches(regex: &Regex, content: &str, anchored: bool) -> Result<bool, ConfigurationError> {
    regex
        .find(content)
        .map(|found| found.is_some_and(|found| !anchored || found.start() == 0))
        .map_err(|_| invalid("Deployment regex exceeded its matching bound"))
}

impl PublishSelection {
    /// Compile filters before service mutations.
    ///
    /// # Errors
    /// Invalid or resource-exhausted regex patterns fail validation.
    pub fn new(
        config: &DeploymentConfig,
        context: &EngineContext,
    ) -> Result<Self, ConfigurationError> {
        let list = |key| {
            config
                .publish
                .get(key)
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
        };
        Ok(Self {
            name: pattern(config.publish.get("exclude_regex"))?,
            folder: pattern(config.publish.get("folder_exclude_regex"))?,
            folders: list("folder_path_to_include"),
            items: list("items_to_include").map(|values| {
                values
                    .into_iter()
                    .map(|value| value.to_lowercase())
                    .collect()
            }),
            folders_disabled: context.enabled("disable_workspace_folder_publish"),
        })
    }

    /// Exact source item selection: case-insensitive include, anchored name regex.
    ///
    /// # Errors
    /// Regex resource failure is never interpreted as a nonmatch.
    pub fn includes(&self, item: &RepositoryItem) -> Result<bool, ConfigurationError> {
        if self.items.as_ref().is_some_and(|items| {
            !items.contains(&format!("{}.{}", item.name, item.item_type).to_lowercase())
        }) {
            return Ok(false);
        }
        if self
            .name
            .as_ref()
            .map(|regex| regex_matches(regex, &item.name, true))
            .transpose()?
            .unwrap_or(false)
        {
            return Ok(false);
        }
        if self.folders_disabled || item.folder_path.is_empty() {
            return Ok(true);
        }
        if let Some(regex) = &self.folder {
            let mut path = item.folder_path.as_str();
            while !path.is_empty() {
                if regex_matches(regex, path, false)? {
                    return Ok(false);
                }
                path = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            }
        }
        Ok(self
            .folders
            .as_ref()
            .is_none_or(|folders| folders.contains(&item.folder_path)))
    }

    /// Folder ancestors are retained for hierarchy; inclusion does not include siblings.
    ///
    /// # Errors
    /// Regex errors remain validation failures.
    pub fn includes_folder(&self, folder: &str) -> Result<bool, ConfigurationError> {
        if self.folders_disabled {
            return Ok(false);
        }
        if let Some(regex) = &self.folder {
            let mut path = folder;
            while !path.is_empty() {
                if regex_matches(regex, path, false)? {
                    return Ok(false);
                }
                path = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            }
        }
        Ok(self.folders.as_ref().is_none_or(|folders| {
            folders.iter().any(|included| {
                included == folder
                    || included
                        .strip_prefix(folder)
                        .is_some_and(|tail| tail.starts_with('/'))
            })
        }))
    }
}

/// Complete pure publication plan; target IDs are resolved by the async workflow.
pub struct DeploymentPlan {
    /// Complete repository, including out-of-scope items needed for logical refs.
    pub repository: DeploymentRepository,
    /// Item indices in source type order and dependency order within a type.
    pub order: Vec<usize>,
    /// Selected nonempty workspace folders, parents first.
    pub folders: Vec<String>,
    /// Source filter policy retained for deferred family hooks.
    pub selection: PublishSelection,
}

impl DeploymentPlan {
    /// Validate processors, source selection, logical references and cycles.
    ///
    /// # Errors
    /// Missing selected dependencies, invalid pipeline JSON and cycles fail before writes.
    pub fn prepare(
        config: &DeploymentConfig,
        context: &EngineContext,
        mut repository: DeploymentRepository,
    ) -> Result<Self, ConfigurationError> {
        let mut bytes = 0usize;
        let mut entries = repository
            .items
            .len()
            .saturating_add(repository.folders.len());
        if entries > 100_000 {
            return Err(ConfigurationError::Limit);
        }
        for item in &repository.items {
            entries = entries
                .checked_add(item.files.len())
                .filter(|entries| *entries <= 100_000)
                .ok_or(ConfigurationError::Limit)?;
            for file in &item.files {
                bytes = bytes
                    .checked_add(file.bytes.len())
                    .filter(|bytes| *bytes <= context.definition_limits.max_bytes.get())
                    .ok_or(ConfigurationError::Limit)?;
            }
        }
        for item in &mut repository.items {
            let pattern = match item.item_type.as_str() {
                "KQLDatabase" => Some(context.text_constant(
                    "KQL_DATABASE_FOLDER_PATH_REGEX",
                    r"(?i)^(.*)/[^/]+\.Eventhouse/\.children(?:/.*)?$",
                )),
                "OrgAppAudience" => Some(context.text_constant(
                    "ORG_APP_AUDIENCE_FOLDER_PATH_REGEX",
                    r"(?i)^(.*)/[^/]+\.OrgApp/\.children(?:/.*)?$",
                )),
                _ => None,
            };
            if let Some(pattern) = pattern {
                let regex = Regex::new(pattern)
                    .map_err(|_| invalid("Invalid parent-managed folder pattern"))?;
                let path = format!("/{}", item.path);
                item.folder_path = regex
                    .captures(&path)
                    .map_err(|_| invalid("Parent-managed folder pattern exceeded its bound"))?
                    .and_then(|captures| captures.get(1).map(|capture| capture.as_str().to_owned()))
                    .unwrap_or_default();
            }
        }
        let selection = PublishSelection::new(config, context)?;
        let mut selected = Vec::new();
        let mut logical = HashMap::new();
        let mut seen_logical = HashSet::new();
        for (index, item) in repository.items.iter().enumerate() {
            Processor::for_type(&item.item_type)?;
            if item.logical_id != context.default_guid() {
                if !seen_logical.insert(item.logical_id.clone()) {
                    return Err(invalid("Duplicate logicalId in repository"));
                }
            }
            logical
                .entry((item.item_type.as_str(), item.logical_id.as_str()))
                .or_insert(index);
            if !config.skip_publish()
                && config.item_types.contains(&item.item_type)
                && selection.includes(item)?
            {
                selected.push(index);
            }
        }
        let mut order = Vec::new();
        for item_type in context.type_order() {
            let indices: Vec<_> = selected
                .iter()
                .copied()
                .filter(|index| repository.items[*index].item_type == *item_type)
                .collect();
            if *item_type == "DataPipeline" && !indices.is_empty() {
                let all_indices: Vec<_> = repository
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.item_type == "DataPipeline")
                    .map(|(index, _)| index)
                    .collect();
                let mut edges = Vec::new();
                for index in &all_indices {
                    let item = &repository.items[*index];
                    let filename = context.dependency_file("DataPipeline")?;
                    let content = item
                        .file(&filename)
                        .ok_or_else(|| invalid("Missing pipeline dependency definition"))?;
                    let content: Value = if filename.ends_with(".json") {
                        serde_json::from_slice(&content.bytes)
                            .map_err(|_| invalid("Invalid pipeline dependency JSON"))?
                    } else {
                        Value::String(content.content()?.into())
                    };
                    for reference in pipeline_references(&content) {
                        if let Some(dependency) = logical.get(&("DataPipeline", reference.as_str()))
                            && repository.items[*dependency].item_type == "DataPipeline"
                        {
                            edges.push((*index, *dependency));
                        }
                    }
                }
                order.extend(
                    topological_order(&all_indices, &edges)?
                        .into_iter()
                        .filter(|index| indices.contains(index)),
                );
            } else {
                order.extend(indices);
            }
        }
        let folders = repository
            .folders
            .iter()
            .filter_map(|folder| match selection.includes_folder(folder) {
                Ok(true) => Some(Ok(folder.clone())),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            repository,
            order,
            folders,
            selection,
        })
    }
}

/// Collect only actual nested ExecutePipeline/InvokePipeline references.
pub fn pipeline_references(content: &Value) -> Vec<String> {
    fn value(value: &Value) -> Option<&str> {
        if value.get("type").and_then(Value::as_str) == Some("Expression") {
            value.get("value").and_then(Value::as_str)
        } else {
            value.as_str()
        }
    }
    fn visit(activities: Option<&Value>, result: &mut Vec<String>) {
        for activity in activities.and_then(Value::as_array).into_iter().flatten() {
            let Some(properties) = activity.get("typeProperties") else {
                continue;
            };
            let reference = match activity.get("type").and_then(Value::as_str) {
                Some("ExecutePipeline") => properties
                    .pointer("/pipeline/referenceName")
                    .and_then(value),
                Some("InvokePipeline") => properties.get("pipelineId").and_then(value),
                _ => None,
            };
            if let Some(reference) =
                reference.filter(|reference| crate::deployment_config::valid_guid(reference))
                && !result.iter().any(|value| value == reference)
            {
                result.push(reference.into());
            }
            for key in [
                "activities",
                "ifTrueActivities",
                "ifFalseActivities",
                "defaultActivities",
            ] {
                visit(properties.get(key), result);
            }
            for case in properties
                .get("cases")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                visit(case.get("activities"), result);
            }
        }
    }
    let mut result = Vec::new();
    visit(content.pointer("/properties/activities"), &mut result);
    result
}

/// Stable source Kahn ordering, with dependency edges `(dependent, prerequisite)`.
///
/// # Errors
/// Cycles or missing prerequisites fail before an operation plan is executed.
pub fn topological_order(
    indices: &[usize],
    edges: &[(usize, usize)],
) -> Result<Vec<usize>, ConfigurationError> {
    let mut indegree: HashMap<_, usize> = indices.iter().copied().map(|index| (index, 0)).collect();
    let mut dependents: HashMap<usize, Vec<usize>> = HashMap::new();
    for (dependent, prerequisite) in edges {
        if !indegree.contains_key(prerequisite) {
            continue;
        }
        *indegree
            .get_mut(dependent)
            .ok_or_else(|| invalid("Missing dependency target"))? += 1;
        dependents
            .entry(*prerequisite)
            .or_default()
            .push(*dependent);
    }
    let mut ready: VecDeque<_> = indices
        .iter()
        .copied()
        .filter(|index| indegree[index] == 0)
        .collect();
    let mut result = Vec::new();
    while let Some(index) = ready.pop_front() {
        result.push(index);
        for dependent in dependents.get(&index).into_iter().flatten() {
            let degree = indegree
                .get_mut(dependent)
                .ok_or_else(|| invalid("Missing dependency target"))?;
            *degree -= 1;
            if *degree == 0 {
                ready.push_back(*dependent);
            }
        }
    }
    if result.len() != indices.len() {
        return Err(invalid("There is a cycle in the dependency graph"));
    }
    Ok(result)
}

pub(crate) fn orphan_selected(
    config: &DeploymentConfig,
    item_type: &str,
    name: &str,
) -> Result<bool, ConfigurationError> {
    if let Some(items) = config
        .unpublish
        .get("items_to_include")
        .and_then(Value::as_array)
    {
        return Ok(items
            .iter()
            .any(|item| item.as_str() == Some(&format!("{name}.{item_type}"))));
    }
    let excluded = pattern(config.unpublish.get("exclude_regex"))?;
    Ok(!excluded
        .as_ref()
        .map(|regex| regex_matches(regex, name, true))
        .transpose()?
        .unwrap_or(name.is_empty()))
}
