// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CommandError, CommandValue, python_truthy};
use fabric_sdk_api::{ApiError, Limits};
use fabric_sdk_catalog::{CatalogClient, SearchOptions};
use serde_json::{Value, json};
use std::{future::Future, num::NonZeroUsize};

/// Source find arguments after nargs normalization, not a new parser.
#[derive(Clone, Debug, Default)]
pub struct FindOptions {
    /// Required search text, passed to the service unchanged.
    pub search: String,
    /// Source single `-P` string.
    pub params: Option<String>,
    /// Include IDs and avoid text truncation.
    pub long: bool,
    /// Already-joined source JMESPath expression.
    pub query: Option<String>,
}

const TYPES: &[&str] = &[
    "AnomalyDetector",
    "ApacheAirflowJob",
    "CopyJob",
    "CosmosDBDatabase",
    "Dataflow",
    "Datamart",
    "DataPipeline",
    "DigitalTwinBuilder",
    "DigitalTwinBuilderFlow",
    "Environment",
    "Eventhouse",
    "EventSchemaSet",
    "Eventstream",
    "GraphModel",
    "GraphQLApi",
    "GraphQuerySet",
    "KQLDashboard",
    "KQLDatabase",
    "KQLQueryset",
    "Lakehouse",
    "Map",
    "MirroredAzureDatabricksCatalog",
    "MirroredDatabase",
    "MirroredWarehouse",
    "MLExperiment",
    "MLModel",
    "MountedDataFactory",
    "Notebook",
    "Ontology",
    "OperationsAgent",
    "PaginatedReport",
    "Reflex",
    "Report",
    "SemanticModel",
    "SnowflakeDatabase",
    "SparkJobDefinition",
    "SQLDatabase",
    "SQLEndpoint",
    "UserDataFunction",
    "VariableLibrary",
    "Warehouse",
    "WarehouseSnapshot",
    "Dashboard",
];

fn parameter_pairs(text: &str) -> Vec<(String, String)> {
    fn key_end(text: &str, start: usize) -> Option<usize> {
        let mut end = start;
        for (offset, ch) in text[start..].char_indices() {
            if ch.is_alphanumeric() || matches!(ch, '_' | '.') {
                end = start + offset + ch.len_utf8();
            } else {
                break;
            }
        }
        if end == start {
            return None;
        }
        if text[end..].starts_with('!') {
            end += 1;
        }
        (text[end..].starts_with('=') && end + 1 < text.len()).then_some(end)
    }
    let mut result: Vec<(String, String)> = Vec::new();
    let mut consumed = 0;
    for (start, _) in text.char_indices() {
        if start < consumed {
            continue;
        }
        let Some(end_key) = key_end(text, start) else {
            continue;
        };
        let value_start = end_key + 1;
        let end = text[value_start..]
            .char_indices()
            .find_map(|(offset, ch)| {
                if ch != ',' {
                    return None;
                }
                let next = value_start + offset + 1;
                let next = next + text[next..].len() - text[next..].trim_start().len();
                key_end(text, next).map(|_| value_start + offset)
            })
            .unwrap_or(text.len());
        let key = &text[start..end_key];
        let value = &text[value_start..end];
        if value.is_empty() {
            continue;
        }
        if let Some(pair) = result.iter_mut().find(|p| p.0 == key) {
            pair.1 = value.into();
        } else {
            result.push((key.into(), value.into()));
        }
        consumed = end;
    }
    result
}

fn type_filter(params: &str) -> Result<Option<String>, CommandError> {
    if params.is_empty() {
        return Ok(None);
    }
    let normalized = params.replace('\\', "");
    let normalized = normalized.trim();
    let pairs = parameter_pairs(normalized);
    if pairs.is_empty() {
        return Err(CommandError::new(
            "InvalidInput",
            format!("Invalid parameter format: '{normalized}'. Use key=value or key!=value"),
        ));
    }
    let mut selected = None;
    for (key, value) in &pairs {
        let clean = key.trim_end_matches('!');
        if !clean.eq_ignore_ascii_case("type") {
            return Err(CommandError::new(
                "InvalidInput",
                format!("'{clean}' isn't a supported parameter"),
            ));
        }
        selected = Some((key.ends_with('!'), value.as_str()));
    }
    let Some((ne, value)) = selected else {
        return Ok(None);
    };
    let values: Vec<_> = if value.starts_with('[') && value.ends_with(']') {
        value[1..value.len() - 1]
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect()
    } else {
        vec![value.trim()]
    };
    if values.is_empty() {
        return Err(CommandError::new(
            "InvalidInput",
            "'[]' isn't a recognized item type. Specify at least one item type.",
        ));
    }
    let mut clauses = Vec::new();
    for value in values {
        if value.eq_ignore_ascii_case("Dashboard") && !ne {
            return Err(CommandError::new(
                "UnsupportedItemType",
                "The type 'Dashboard' is not supported",
            ));
        }
        let canonical = TYPES
            .iter()
            .find(|t| t.eq_ignore_ascii_case(value))
            .ok_or_else(|| {
                let lower = value.to_lowercase();
                let close: Vec<_> = TYPES
                    .iter()
                    .filter(|t| {
                        t.to_lowercase().contains(&lower) || lower.contains(&t.to_lowercase())
                    })
                    .copied()
                    .collect();
                let hint = if close.is_empty() {
                    " Run 'find --help' to see examples.".into()
                } else {
                    format!(" Did you mean [{}]?", close.join(","))
                };
                CommandError::new(
                    "InvalidItemType",
                    format!("'{value}' isn't a recognized item type.{hint}"),
                )
            })?;
        clauses.push(format!(
            "Type {} '{canonical}'",
            if ne { "ne" } else { "eq" }
        ));
    }
    let joined = clauses.join(if ne { " and " } else { " or " });
    Ok(Some(if clauses.len() > 1 {
        format!("({joined})")
    } else {
        joined
    }))
}

/// Builds the pinned initial search payload, including source type normalization.
/// Interactive callers use 30; command-line callers use 1000.
pub fn build_search_options(
    options: &FindOptions,
    interactive: bool,
) -> Result<SearchOptions, CommandError> {
    Ok(SearchOptions {
        search: options.search.clone(),
        page_size: NonZeroUsize::new(if interactive { 30 } else { 1000 }).unwrap(),
        filter: options
            .params
            .as_deref()
            .map(type_filter)
            .transpose()?
            .flatten(),
    })
}

/// Source catalog projection, including hierarchy-before-flat workspace fields
/// and description-column presence based on all unfiltered rows.
pub fn project_search(rows: &[Value], options: &FindOptions) -> Result<Vec<Value>, CommandError> {
    let descriptions = rows
        .iter()
        .any(|r| r.get("description").is_some_and(python_truthy));
    let choose = |a: Option<&Value>, b: Option<&Value>| {
        a.filter(|v| python_truthy(v))
            .or(b)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut display = Vec::new();
    for row in rows {
        if !row.is_object()
            || row.get("hierarchy").is_some_and(|v| !v.is_object())
            || row
                .get("hierarchy")
                .and_then(|v| v.get("workspace"))
                .is_some_and(|v| !v.is_object())
        {
            return Err(CommandError::new(
                "InvalidEntriesFormat",
                "Invalid catalog entries format",
            ));
        }
        let workspace = &row["hierarchy"]["workspace"];
        let name = choose(row.get("displayName"), row.get("name"));
        let ws = choose(workspace.get("displayName"), row.get("workspaceName"));
        let mut entry = if options.long {
            json!({"name":name,"id":row.get("id"),"type":row.get("type"),"workspace":ws,"workspace_id":choose(workspace.get("id"),row.get("workspaceId"))})
        } else {
            json!({"name":name,"type":row.get("type"),"workspace":ws})
        };
        if descriptions {
            entry["description"] =
                choose(row.get("description"), Some(&Value::String(String::new())));
        }
        display.push(entry);
    }
    if let Some(query) = options.query.as_deref().filter(|q| !q.is_empty()) {
        let result = fabric_cli_query::search(&Value::Array(display), query).map_err(|_| {
            CommandError::new(
                "InvalidInput",
                "Invalid jmespath query (https://jmespath.org)",
            )
        })?;
        return Ok(result.as_array().cloned().unwrap_or_default());
    }
    Ok(display)
}

/// Executes the real command-line find SDK/projection/output consumer.
/// Interactive hosts use `CatalogClient::search_page` and the same projection.
pub async fn find(
    client: &CatalogClient,
    options: &FindOptions,
    limits: Limits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    let request = build_search_options(options, false)?;
    let progress = client
        .search(&request, limits, cancelled)
        .await
        .map_err(|e| {
            let error = match e.error {
                ApiError::Service { code, message, .. } => {
                    CommandError::new(&code, format!("Catalog search failed: {message}"))
                }

                error => CommandError::from(error),
            };
            error.with_context(&CommandValue {
                diagnostics: vec!["Searching...".into()],
                ..CommandValue::default()
            })
        })?;
    let rows = project_search(&progress.rows, options).map_err(|e| {
        e.with_context(&CommandValue {
            diagnostics: vec!["Searching...".into()],
            ..CommandValue::default()
        })
    })?;
    if rows.is_empty() {
        return Ok(CommandValue {
            diagnostics: vec!["Searching...".into(), "No items found.".into()],
            ..CommandValue::default()
        });
    }
    let count = rows.len();
    Ok(CommandValue {
        data: Some(Value::Array(rows)),
        diagnostics: vec![
            "Searching...".into(),
            String::new(),
            format!(
                "{count} {} found",
                if count == 1 { "item" } else { "items" }
            ),
            String::new(),
            String::new(),
        ],
        ..CommandValue::default()
    })
}
