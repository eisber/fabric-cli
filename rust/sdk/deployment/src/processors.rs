// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned publisher dispatch; generic processing is used only for generic source publishers.

use crate::{
    deployment_config::ConfigurationError,
    model::{RepositoryItem, invalid},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

/// Actual source publisher lifecycle, not an item-import support claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Processor {
    /// Source inherits ItemPublisher.publish_one without extra lifecycle.
    Definition,
    /// Metadata-only create/PATCH, not definition publication.
    Shell,
    /// Parameter-only definition followed by active value-set PATCH.
    VariableLibrary,
    /// Optional warehouse creationPayload, then metadata-only updates.
    Warehouse,
    /// Schemas at creation, SQL endpoint readiness and shortcut reconciliation.
    Lakehouse,
    /// Spark pool replacement, staging publication and asynchronous state checks.
    Environment,
    /// Parent definition excludes separately published `.children`.
    Parent,
    /// Definitions exclude private `.pbi` files.
    ExcludePbi,
    /// Content-before-settings ordering and optional ipynb format.
    Notebook,
    /// Semantic model purge-data options and connection binding.
    SemanticModel,
    /// byPath semantic-model connection conversion.
    Report,
    /// SparkJobDefinitionV2 format.
    SparkJob,
    /// Repository logical database lookup and query URI fixup.
    KqlQueryset,
    /// Deployed database name lookup and query URI fixup.
    KqlDashboard,
    /// Parameter-derived source dataflow ordering and ID fixup.
    Dataflow,
    /// Nested activity-only dependency ordering, including orphan deletion.
    DataPipeline,
}

impl Processor {
    /// Resolve exactly the types admitted by the source factory.
    ///
    /// # Errors
    /// Unknown types remain actual Python-equivalent validation errors.
    pub fn for_type(item_type: &str) -> Result<Self, ConfigurationError> {
        Ok(match item_type {
            "VariableLibrary" => Self::VariableLibrary,
            "Warehouse" => Self::Warehouse,
            "Lakehouse" => Self::Lakehouse,
            "Environment" => Self::Environment,
            "MLExperiment" | "SQLDatabase" => Self::Shell,
            "Eventhouse" | "OrgApp" => Self::Parent,
            "DataAgent" => Self::ExcludePbi,
            "Notebook" => Self::Notebook,
            "SemanticModel" => Self::SemanticModel,
            "Report" => Self::Report,
            "SparkJobDefinition" => Self::SparkJob,
            "KQLQueryset" => Self::KqlQueryset,
            "KQLDashboard" => Self::KqlDashboard,
            "Dataflow" => Self::Dataflow,
            "DataPipeline" => Self::DataPipeline,
            "MirroredDatabase" | "CosmosDBDatabase" | "UserDataFunction" | "PaginatedReport"
            | "CopyJob" | "DataBuildToolJob" | "KQLDatabase" | "Reflex" | "Eventstream"
            | "GraphQLApi" | "ApacheAirflowJob" | "MountedDataFactory" | "Ontology" | "Map"
            | "GraphModel" | "OrgAppAudience" => Self::Definition,
            _ => {
                return Err(invalid(
                    "Item type is not supported by the pinned Python source",
                ));
            }
        })
    }

    /// Whether source publication uses only metadata, including on updates.
    pub fn shell_only(self) -> bool {
        matches!(self, Self::Shell | Self::Warehouse | Self::Lakehouse)
    }

    /// Source path exclusion, evaluated against the complete item-relative path.
    pub fn excludes(self, path: &str) -> bool {
        let container = match self {
            Self::Parent => ".children",
            Self::Report | Self::SemanticModel | Self::ExcludePbi => ".pbi",
            _ => return false,
        };
        path.split(['/', '\\'])
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair[0].ends_with(container))
    }

    /// Source optional creation payload; absent is distinct from an empty object.
    ///
    /// # Errors
    /// Malformed source warehouse `.platform` fails before a write.
    pub fn creation_payload(
        self,
        item: &RepositoryItem,
    ) -> Result<Option<Value>, ConfigurationError> {
        if self == Self::Lakehouse {
            return Ok(item
                .files
                .iter()
                .any(|file| {
                    file.name() == "lakehouse.metadata.json"
                        && file
                            .content()
                            .is_ok_and(|content| content.contains("defaultSchema"))
                })
                .then(|| json!({"enableSchemas": true})));
        }
        if self == Self::Warehouse {
            let platform = item
                .file(".platform")
                .ok_or_else(|| invalid("Missing .platform"))?;
            let platform: Value = serde_json::from_slice(&platform.bytes)
                .map_err(|_| invalid("Invalid .platform JSON"))?;
            return Ok(platform
                .pointer("/metadata/creationPayload")
                .filter(|value| {
                    !value.is_null() && value.as_object().is_none_or(|object| !object.is_empty())
                })
                .cloned());
        }
        Ok(None)
    }
}

/// Encode actual selected parts in pinned source order.
///
/// The caller has already run family-specific processing and the ordered text
/// pipeline. Binary bytes and `.platform` bypass that pipeline.
pub fn definition(processor: Processor, item: &RepositoryItem) -> Value {
    let mut files: Vec<_> = item
        .files
        .iter()
        .filter(|file| !processor.excludes(&file.path))
        .collect();
    if processor == Processor::Notebook {
        files.sort_by_key(|file| {
            let priority = if file.name() == ".platform" {
                0
            } else if file.name().ends_with(".py") || file.name().ends_with(".ipynb") {
                1
            } else if file.name().ends_with(".json") {
                3
            } else {
                2
            };
            (priority, file.name())
        });
    }
    let parts: Vec<_> = files.iter().map(|file| json!({
        "path": file.path, "payload": STANDARD.encode(&file.bytes), "payloadType": "InlineBase64",
    })).collect();
    let format = match processor {
        Processor::SparkJob => Some("SparkJobDefinitionV2"),
        Processor::Notebook if files.iter().any(|file| file.name().ends_with(".ipynb")) => {
            Some("ipynb")
        }
        _ => None,
    };
    match format {
        Some(format) => json!({"format": format, "parts": parts}),
        None => json!({"parts": parts}),
    }
}

/// Exact source create/update metadata and definition body.
///
/// # Errors
/// Invalid creation payload metadata fails before dispatch.
pub fn mutation_body(
    processor: Processor,
    item: &RepositoryItem,
    existing: bool,
    folder_id: &str,
    purge_data: bool,
) -> Result<Value, ConfigurationError> {
    mutation_body_with_policy(
        processor,
        item,
        existing,
        folder_id,
        purge_data,
        processor.shell_only(),
        None,
    )
}

/// Source mutation body with an explicit invocation-local shell-only policy.
///
/// # Errors
/// Retains malformed creation metadata before request dispatch.
pub fn mutation_body_with_policy(
    processor: Processor,
    item: &RepositoryItem,
    existing: bool,
    folder_id: &str,
    purge_data: bool,
    shell_only: bool,
    definition_override: Option<Value>,
) -> Result<Value, ConfigurationError> {
    if existing {
        if shell_only {
            return Ok(json!({"displayName": item.name, "description": item.description}));
        }
        let mut body = json!({"definition": definition_override.unwrap_or_else(|| definition(processor, item))});
        if processor == Processor::SemanticModel && purge_data {
            body["options"] = json!({"allowPurgeData": true});
        }
        return Ok(body);
    }
    let mut body =
        json!({"displayName": item.name, "type": item.item_type, "description": item.description});
    if let Some(payload) = processor.creation_payload(item)? {
        body["creationPayload"] = payload;
    } else if !shell_only {
        body["definition"] = definition_override.unwrap_or_else(|| definition(processor, item));
    }
    body["folderId"] = folder_id.into();
    Ok(body)
}

/// Source-selected definition format and exclusion policy within one invocation.
///
/// # Errors
/// Invalid configured source patterns fail before dispatch instead of being ignored.
pub fn definition_with_context(
    processor: Processor,
    item: &RepositoryItem,
    context: &crate::planner::EngineContext,
    bulk: bool,
) -> Result<Value, ConfigurationError> {
    let mut prepared = item.clone();
    prepared.files.clear();
    for file in &item.files {
        if !excludes_with_context(processor, &item.item_type, &file.path, context, bulk)? {
            prepared.files.push(file.clone());
        }
    }
    let mut definition = definition(
        if bulk {
            Processor::Definition
        } else if processor == Processor::Notebook {
            Processor::Notebook
        } else {
            Processor::Definition
        },
        &prepared,
    );
    if matches!(processor, Processor::SparkJob)
        || (processor == Processor::Notebook
            && item
                .files
                .iter()
                .any(|file| file.name().ends_with(".ipynb")))
    {
        let format = context
            .constants
            .get("API_FORMAT_MAPPING")
            .and_then(Value::as_object)
            .map(|mapping| mapping.get(&item.item_type).cloned())
            .unwrap_or_else(|| match processor {
                Processor::SparkJob => Some("SparkJobDefinitionV2".into()),
                Processor::Notebook => Some("ipynb".into()),
                _ => None,
            });
        if let Some(format) = format.filter(|value| {
            !value.is_null() && value.as_str() != Some("") && value.as_bool() != Some(false)
        }) {
            let parts = definition["parts"].take();
            definition = json!({"format":format,"parts":parts});
        } else if let Some(mapping) = definition.as_object_mut() {
            mapping.remove("format");
        }
    }
    Ok(definition)
}

pub(crate) fn excludes_with_context(
    processor: Processor,
    item_type: &str,
    path: &str,
    context: &crate::planner::EngineContext,
    bulk: bool,
) -> Result<bool, ConfigurationError> {
    if !bulk
        && !matches!(
            processor,
            Processor::Parent
                | Processor::Report
                | Processor::SemanticModel
                | Processor::ExcludePbi
        )
    {
        return Ok(false);
    }
    if let Some(mapping) = context
        .constants
        .get("EXCLUDE_PATH_REGEX_MAPPING")
        .and_then(Value::as_object)
    {
        let Some(pattern) = mapping.get(item_type) else {
            return if bulk {
                Ok(false)
            } else {
                Err(invalid(
                    "Source publisher exclusion pattern was removed by configuration",
                ))
            };
        };
        let pattern = pattern
            .as_str()
            .ok_or_else(|| invalid("Source exclusion pattern must be a string"))?;
        let pattern = fancy_regex::Regex::new(pattern)
            .map_err(|_| invalid("Invalid source exclusion regex"))?;
        return pattern
            .find(path)
            .map(|found| found.is_some_and(|found| found.start() == 0))
            .map_err(|_| invalid("Source exclusion regex exceeded its matching bound"));
    }
    Ok(processor.excludes(path))
}
