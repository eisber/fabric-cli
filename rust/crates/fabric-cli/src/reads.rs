// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    Projection, errors,
    parser::{ReadCommand, ReadKind},
};
use fabric_cli_context::{
    Element, HomeContext, ItemType, Lookup, NodeKind, PathInput, ResolveOptions, Resolver,
    WorkspaceType,
};
use fabric_cli_output::{
    CommandError, HiddenItems, Indent, OutputEnvelope, OutputError, OutputInput, OutputStream,
    RenderPlan, Timestamp,
};
use fabric_sdk::{
    Client,
    capacities::{CapacityExecutor, CapacityListLimits},
    items::{ItemCollection, ItemExecutor, ItemGetOptions, ItemListOptions},
    workspaces::{WorkspaceExecutor, WorkspaceListLimits},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    io::{self, Write},
    pin::pin,
    sync::Arc,
    task::Poll,
};

/// Python's configured item sorting modes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ItemSort {
    /// Lowercase short name, then Python enum member name.
    #[default]
    ByName,
    /// Python enum member name, then lowercase short name.
    ByType,
}

/// Explicit configuration snapshot; this library never reads global configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadConfig {
    /// Python `show_hidden`; `--all` also enables hidden collection names.
    pub show_hidden: bool,
    /// Python `output_item_sort_criteria`.
    pub item_sort: ItemSort,
    /// Folder presentation is recognized but requires the separate folder slice.
    pub folder_listing_enabled: bool,
}

/// Output failures, never converted into successful command results.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// A malformed output input could not be rendered.
    #[error(transparent)]
    Output(#[from] OutputError),
    /// A caller-owned stream failed, possibly after a partial write.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A caller-configured, runtime-neutral read-command host.
///
/// `lookup` is a preloaded immutable metadata snapshot, never synchronous network
/// I/O. Use SDK calls outside `Lookup` to populate it. The host neither mutates
/// current/home nor owns a cache, identity backend, transport, clock or runtime.
pub struct ReadHost<'a> {
    /// Real SDK facade, with the host's explicitly selected credential.
    pub sdk: &'a Client,
    /// Workspace operation execution (a shared `HttpExecutor` can supply all three).
    pub workspaces: &'a dyn WorkspaceExecutor,
    /// Generic item execution.
    pub items: &'a dyn ItemExecutor,
    /// Capacity execution for root `ls --long` enrichment.
    pub capacities: &'a dyn CapacityExecutor,
    /// Already-loaded workspace/folder/item lookup metadata.
    pub lookup: &'a dyn Lookup,
    /// Immutable current Fabric context.
    pub current: Arc<Element>,
    /// Explicit home context; no ambient identity discovery.
    pub home: HomeContext,
    /// Host-supplied output timestamp.
    pub timestamp: Timestamp,
    /// Host-supplied configuration snapshot.
    pub config: ReadConfig,
    /// Query/path-discovery implementation, called only at source query boundaries.
    pub projection: &'a dyn Projection,
}

impl ReadHost<'_> {
    /// Select the built-in native query provider, including default GET path discovery.
    ///
    /// This replaces only projection; credentials, executors, context, clock,
    /// configuration, runtime and streams remain owned by the caller.
    pub fn with_default_projection(self) -> Self {
        Self {
            projection: &crate::PythonQuery,
            ..self
        }
    }

    /// Execute a read through the SDK and render a Python JSON envelope.
    ///
    /// Service/context/projection errors become ordinary failed command envelopes
    /// on stdout (Unauthorized exits 4, other failures 1). Explicit cooperative
    /// cancellation is a command failure, not main's keyboard-interrupt exit 2.
    /// Partial SDK pages are never emitted as a successful complete listing.
    pub async fn dispatch(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RenderPlan, OutputError> {
        let result = self.execute(command, cancelled).await;
        match result {
            Ok(Some(input)) => {
                OutputEnvelope::new(self.timestamp.clone(), input).render(Indent::Four)
            }
            Ok(None) => Ok(RenderPlan {
                stream: OutputStream::Stdout,
                exit_code: 0,
                payload: Vec::new(),
            }),
            Err(error) => error
                .envelope(self.timestamp.clone(), Some(command.name.clone()))
                .render(Indent::Four),
        }
    }

    /// Dispatch and write to caller-owned streams, returning the product exit code.
    ///
    /// Writers are not flushed, retried, replaced or hidden on failure.
    pub async fn run(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<u8, WriteError> {
        let plan = self.dispatch(command, cancelled).await?;
        plan.write_to(stdout, stderr)?;
        Ok(plan.exit_code)
    }

    async fn execute(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<OutputInput>, CommandError> {
        let mut cancelled = pin!(cancelled);
        let was_cancelled = poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await;
        if was_cancelled {
            return Err(errors::error(
                "LongRunningOperationCancelled",
                "Operation cancelled",
            ));
        }
        if command
            .output_format
            .as_deref()
            .is_some_and(|format| format != "json")
        {
            return Err(errors::unsupported(
                "Only JSON output is implemented for bounded reads",
            ));
        }
        if command.output.is_some() || command.force || command.verbose {
            return Err(errors::unsupported(
                "GET export, verbose and definition/confirmation options are not ported",
            ));
        }
        let tokens: Vec<_> = command.path.iter().map(String::as_str).collect();
        let resolver = Resolver::new(self.lookup, Arc::clone(&self.current), self.home.clone());
        let target = resolver
            .resolve(PathInput::Tokens(&tokens), ResolveOptions::default())
            .map_err(|error| errors::error(&error.code, error.message))?;
        let show_hidden = command.all || self.config.show_hidden;
        let (data, hidden_data) = match (command.kind, target.kind()) {
            (ReadKind::List, NodeKind::Tenant) => {
                let list = self
                    .sdk
                    .list_workspaces(
                        self.workspaces,
                        WorkspaceListLimits::default(),
                        cancelled.as_mut(),
                    )
                    .await
                    .map_err(|error| errors::workspace(&error.kind))?;
                let mut rows = Vec::new();
                for workspace in &list.value {
                    let kind = match workspace.workspace_type.to_lowercase().as_str() {
                        "workspace" => WorkspaceType::Workspace,
                        "personal" => WorkspaceType::Personal,
                        _ => continue,
                    };
                    let element = Element::child(
                        &workspace.display_name,
                        Some(workspace.id.clone()),
                        NodeKind::Workspace(kind),
                        Arc::clone(&target),
                    )
                    .map_err(|error| errors::error(&error.code, error.message))?;
                    rows.push(json!({"name": element.name(), "id": workspace.id}));
                }
                rows.sort_by_cached_key(|row| {
                    row["name"].as_str().unwrap_or_default().to_lowercase()
                });
                if command.long && !rows.is_empty() {
                    let capacities = self
                        .sdk
                        .list_capacities(
                            self.capacities,
                            CapacityListLimits::default(),
                            cancelled.as_mut(),
                        )
                        .await
                        .map_err(|error| errors::capacity(&error.kind))?;
                    let capacities: BTreeMap<_, _> = capacities
                        .value
                        .iter()
                        .map(|capacity| (capacity.id.as_str(), capacity))
                        .collect();
                    let details: BTreeMap<_, _> = list
                        .value
                        .iter()
                        .map(|workspace| (workspace.id.as_str(), workspace))
                        .collect();
                    for row in &mut rows {
                        let workspace = details[row["id"].as_str().expect("SDK IDs are strings")];
                        let capacity_id = workspace
                            .additional_fields
                            .get("capacityId")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let capacity = capacity_id.as_str().and_then(|id| capacities.get(id));
                        row["capacityId"] = capacity_id;
                        row["capacityName"] =
                            capacity.map_or_else(|| json!("N/A"), |c| json!(c.display_name));
                        row["capacityRegion"] = capacity.map_or_else(
                            || json!("Unknown"),
                            |c| c.region().cloned().unwrap_or_else(|| json!("Unknown")),
                        );
                    }
                }
                let columns: &[&str] = if command.long {
                    &["name", "id", "capacityName", "capacityId", "capacityRegion"]
                } else {
                    &["name"]
                };
                let data = self.list_projection(rows, columns, command)?;
                let hidden = show_hidden
                    .then(|| hidden(&[".capacities", ".connections", ".gateways", ".domains"]));
                (data, hidden)
            }
            (ReadKind::List, NodeKind::Workspace(_)) => {
                if self.config.folder_listing_enabled {
                    return Err(errors::unsupported(
                        "Folder listing requires the separate folder composition",
                    ));
                }
                let list = self
                    .sdk
                    .list_items(
                        self.items,
                        required_id(&target)?,
                        ItemListOptions::default(),
                        cancelled.as_mut(),
                    )
                    .await
                    .map_err(|error| errors::item(&error.kind))?;
                let mut elements = Vec::new();
                for item in list.value {
                    if !item.additional_fields.contains_key("type") {
                        return Err(errors::error(
                            "InvalidResponse",
                            "Fabric returned an item without its type",
                        ));
                    }
                    let Some(kind) = item
                        .item_type()
                        .and_then(|value| ItemType::parse(value).ok())
                    else {
                        continue;
                    };
                    let element = Element::child(
                        &item.display_name,
                        Some(item.id),
                        NodeKind::Item(kind),
                        Arc::clone(&target),
                    )
                    .map_err(|error| errors::error(&error.code, error.message))?;
                    elements.push((element, enum_name(kind)));
                }
                elements.sort_by_cached_key(|(element, kind)| {
                    let name = element.short_name().to_lowercase();
                    match self.config.item_sort {
                        ItemSort::ByName => (name, (*kind).to_owned()),
                        ItemSort::ByType => ((*kind).to_owned(), name),
                    }
                });
                let rows = elements
                    .iter()
                    .map(|(element, _)| {
                        if command.long {
                            json!({"name": element.name(), "id": element.id()})
                        } else {
                            json!({"name": element.name()})
                        }
                    })
                    .collect();
                let columns: &[&str] = if command.long {
                    &["name", "id"]
                } else {
                    &["name"]
                };
                let data = self.list_projection(rows, columns, command)?;
                let hidden = show_hidden.then(|| {
                    hidden(&[
                        ".sparkpools",
                        ".managedidentities",
                        ".managedprivateendpoints",
                        ".externaldatashares",
                    ])
                });
                (data, hidden)
            }
            (ReadKind::Get, NodeKind::Workspace(_)) => {
                let Some(data) = self
                    .workspace_get(&target, command, cancelled.as_mut())
                    .await?
                else {
                    return Ok(None);
                };
                (data, None)
            }
            (ReadKind::Get, NodeKind::Item(kind)) => {
                if unsupported_get(kind) {
                    return Err(errors::error(
                        "UnsupportedCommand",
                        "not supported for command 'get'",
                    ));
                }
                let collection =
                    ItemCollection::from_item_type(kind.as_str()).ok_or_else(|| {
                        errors::error("UnsupportedCommand", "not supported for command 'get'")
                    })?;
                let workspace = target
                    .workspace()
                    .expect("item parentage is validated by context");
                let item = self
                    .sdk
                    .get_item_metadata_from_collection(
                        self.items,
                        required_id(&workspace)?,
                        collection,
                        required_id(&target)?,
                        ItemGetOptions::default(),
                        cancelled.as_mut(),
                    )
                    .await
                    .map_err(|error| errors::item(&error.kind))?;
                let data = item.into_value();
                let data = self.projection.project(
                    data,
                    command.query.as_deref().filter(|query| !query.is_empty()),
                    false,
                )?;
                (data, None)
            }
            (ReadKind::Get, NodeKind::Tenant) => {
                return Err(errors::error(
                    "UnsupportedCommand",
                    "not supported for command 'get'",
                ));
            }
            _ => {
                return Err(errors::unsupported(
                    "This preview supports tenant/workspace LS and generic item GET only",
                ));
            }
        };
        Ok(Some(OutputInput {
            command: Some(command.name.clone()),
            data: Some(data),
            hidden_data,
            ..OutputInput::default()
        }))
    }

    pub(crate) fn list_projection(
        &self,
        rows: Vec<Value>,
        columns: &[&str],
        command: &ReadCommand,
    ) -> Result<Value, CommandError> {
        if let Some(query) = command.query.as_deref().filter(|query| !query.is_empty()) {
            return self
                .projection
                .project(Value::Array(rows), Some(query), false);
        }
        Ok(Value::Array(
            rows.into_iter()
                .map(|row| {
                    Value::Object(
                        columns
                            .iter()
                            .filter_map(|&key| {
                                row.get(key).cloned().map(|value| (key.to_owned(), value))
                            })
                            .collect(),
                    )
                })
                .collect(),
        ))
    }
}

fn hidden(values: &[&str]) -> HiddenItems {
    HiddenItems::Strings(values.iter().map(|value| (*value).to_owned()).collect())
}

pub(crate) fn required_id(element: &Element) -> Result<&str, CommandError> {
    element
        .id()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| errors::error("NotFound", "The requested resource could not be found"))
}

fn unsupported_get(kind: ItemType) -> bool {
    matches!(
        kind.as_str(),
        "Dashboard"
            | "PaginatedReport"
            | "SQLEndpoint"
            | "MirroredWarehouse"
            | "Datamart"
            | "OrgApp"
            | "MetricSet"
            | "ApacheAirflowJob"
            | "Exploration"
            | "RetailDataManager"
            | "Healthcaredatasolution"
            | "SustainabilityDataSolution"
            | "AISkill"
    )
}

// Python sorts on enum member names, not the mixed-case REST type strings.
pub(crate) fn enum_name(kind: ItemType) -> &'static str {
    match kind.as_str() {
        "AISkill" => "AISKILL",
        "ApacheAirflowJob" => "APACHE_AIRFLOW_JOB",
        "RetailDataManager" => "RETAIL_DATA_MANAGER",
        "Healthcaredatasolution" => "HEALTHCARE_DATA_SOLUTION",
        "MetricSet" => "METRIC_SET",
        "OrgApp" => "ORG_APP",
        "SustainabilityDataSolution" => "SUSTAINABILITY_DATA_SOLUTION",
        "CosmosDBDatabase" => "COSMOS_DB_DATABASE",
        "DataPipeline" => "DATA_PIPELINE",
        "DigitalTwinBuilder" => "DIGITAL_TWIN_BUILDER",
        "DigitalTwinBuilderFlow" => "DIGITAL_TWIN_BUILDER_FLOW",
        "GraphModel" => "GRAPH_MODEL",
        "KQLDashboard" => "KQL_DASHBOARD",
        "KQLDatabase" => "KQL_DATABASE",
        "KQLQueryset" => "KQL_QUERYSET",
        "MirroredWarehouse" => "MIRRORED_WAREHOUSE",
        "MirroredDatabase" => "MIRRORED_DATABASE",
        "MLExperiment" => "ML_EXPERIMENT",
        "MLModel" => "ML_MODEL",
        "PaginatedReport" => "PAGINATED_REPORT",
        "SemanticModel" => "SEMANTIC_MODEL",
        "SparkJobDefinition" => "SPARK_JOB_DEFINITION",
        "SQLEndpoint" => "SQL_ENDPOINT",
        "CopyJob" => "COPYJOB",
        "GraphQLApi" => "GRAPHQLAPI",
        "GraphQuerySet" => "GRAPH_QUERY_SET",
        "UserDataFunction" => "USER_DATA_FUNCTION",
        "MountedDataFactory" => "MOUNTED_DATA_FACTORY",
        "SQLDatabase" => "SQL_DATABASE",
        "VariableLibrary" => "VARIABLE_LIBRARY",
        "Exploration" => "EXPLORATION",
        "Dashboard" => "DASHBOARD",
        "Datamart" => "DATAMART",
        "Environment" => "ENVIRONMENT",
        "Eventhouse" => "EVENTHOUSE",
        "Eventstream" => "EVENTSTREAM",
        "Lakehouse" => "LAKEHOUSE",
        "Map" => "MAP",
        "Notebook" => "NOTEBOOK",
        "Ontology" => "ONTOLOGY",
        "Reflex" => "REFLEX",
        "Report" => "REPORT",
        "Warehouse" => "WAREHOUSE",
        "Dataflow" => "DATAFLOW",
        _ => unreachable!("ItemType is a closed validated context type"),
    }
}
