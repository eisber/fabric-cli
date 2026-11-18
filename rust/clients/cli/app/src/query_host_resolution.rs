// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Authoritative query-owner and native Kusto binding resolution over existing SDK reads.

use super::{
    BoundTarget,
    connection::{QueryConnection, QueryConnectionError},
};
use crate::{
    get_resolution::{GetResolutionError, ResolvedGetTarget},
    get_target::{GetPathError, GetRepresentation, GetTarget},
};
use fabric_cli_query_commands::{Artifact, DataAgentRoute, KqlRoute, QueryTarget, Stage};
use fabric_sdk::{
    Client,
    dataagents::{
        ContinuationToken, DataAgentError, DataAgentRequests, DataAgentTarget, DatasourceMetadata,
        Stage as MetadataStage,
    },
    items::{ItemError, ItemExecutor, ItemGetOptions},
    kql_databases::{DatabaseSelector, KqlDatabaseRequests, KqlError, KustoResource},
    representations::RepresentationLimits,
    v2::{ItemIdentity, ResolvedTarget, Target, TargetKind},
    workspaces::WorkspaceExecutor,
};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use fabric_sdk_query::{EngineBinding, QueryError};
use std::{collections::BTreeSet, future::Future, num::NonZeroUsize, pin::pin, time::Duration};

#[cfg(test)]
#[path = "query_host_resolution_tests.rs"]
mod tests;

/// One real item resolved and type-checked through the existing metadata APIs.
pub struct QueryOwner {
    /// Original query path, not a new datasource or provider path grammar.
    pub path: String,
    /// Concrete requested owner; no display name becomes a request identifier.
    pub artifact: Artifact,
    /// Exact service type, checked against any explicitly typed path.
    pub owner_type: String,
}

/// Typed metadata failures, without supplied paths, query text or provider bodies.
#[derive(Debug, thiserror::Error)]
pub enum QueryResolutionError {
    /// Existing path parser rejected the input before metadata access.
    #[error(transparent)]
    Path(#[from] GetPathError),
    /// Only actual DataAgent owners and artifact-native KQL databases are supported.
    #[error("Query requires an actual DataAgent or KQLDatabase owner; no provider is inferred")]
    UnsupportedOwner,
    /// Requested owner/response identity or declared type did not match.
    #[error("Query metadata did not establish the exact requested owner")]
    OwnerMismatch,
    /// Existing bounded name/identifier resolution failed.
    #[error("Authoritative query target resolution did not complete")]
    Resolution(#[source] Box<GetResolutionError>),
    /// Generic metadata validation or service access failed.
    #[error("Query owner metadata could not be read")]
    Item(#[source] Box<ItemError>),
    /// Exact KQL metadata/context construction failed; no alternate route is tried.
    #[error("The KQL database metadata did not establish a supported native binding")]
    Kql(#[source] Box<KqlError>),
    /// The actual native query binding rejected the descriptor.
    #[error("The native KQL query binding is invalid")]
    Binding(#[source] Box<QueryError>),
    /// Connection metadata did not bind the exact DataAgent datasource and stage.
    #[error(transparent)]
    Connection(#[from] QueryConnectionError),
    /// The actual explicit-stage datasource read failed.
    #[error("The configured DataAgent datasource metadata could not be read")]
    DataAgent(#[source] Box<DataAgentError>),
    /// Bounds, continuation or uniqueness did not establish a complete selection.
    #[error("DataAgent datasource resolution is incomplete or ambiguous within its finite bounds")]
    IncompleteDatasources,
    /// No exact supported Fabric item reference exists; no datasource-ID alias is invented.
    #[error(
        "The configured datasource does not establish one exact supported Fabric item reference"
    )]
    UnsupportedDatasource,
}

/// Validate existing owner-path syntax without credentials, input or metadata I/O.
///
/// No query-specific facet, namespace, implicit stage or engine is invented.
pub fn parse_owner(path: &str) -> Result<GetTarget, QueryResolutionError> {
    if path.len() > 16_384 {
        return Err(QueryResolutionError::OwnerMismatch);
    }
    let target = GetTarget::parse(path)?;
    match &target {
        GetTarget::Item {
            item_type,
            representation: GetRepresentation::Metadata,
            ..
        } if item_type
            .as_deref()
            .is_none_or(|kind| matches!(kind, "KQLDatabase" | "DataAgent")) =>
        {
            Ok(target)
        }
        _ => Err(QueryResolutionError::UnsupportedOwner),
    }
}

/// Resolve one owner through existing complete bounded listings and metadata GET.
///
/// All reads use the caller's selected Fabric client/executor. A generic ID path
/// must establish its real owner type; it is never tried against successive
/// provider endpoints. The same cancellation future spans every metadata phase.
pub async fn resolve_owner<E>(
    path: &str,
    client: &Client,
    executor: &E,
    max_bytes: NonZeroUsize,
    canceled: impl Future<Output = ()> + Send,
) -> Result<QueryOwner, QueryResolutionError>
where
    E: ItemExecutor + WorkspaceExecutor,
{
    let target = parse_owner(path)?;
    let mut canceled = pin!(canceled);
    let resolved = crate::get_resolution::resolve(
        target,
        client,
        executor,
        executor,
        max_bytes,
        canceled.as_mut(),
    )
    .await
    .map_err(|error| QueryResolutionError::Resolution(Box::new(error)))?;
    let ResolvedGetTarget::Item {
        workspace_id,
        item_id,
        item_type,
        representation: GetRepresentation::Metadata,
    } = resolved
    else {
        return Err(QueryResolutionError::UnsupportedOwner);
    };

    let metadata = client
        .get_item_metadata(
            executor,
            &workspace_id,
            &item_id,
            ItemGetOptions {
                query: Vec::new(),
                max_response_bytes: max_bytes,
            },
            canceled.as_mut(),
        )
        .await
        .map_err(|error| QueryResolutionError::Item(Box::new(error)))?;
    if !metadata.id().eq_ignore_ascii_case(&item_id)
        || metadata.as_value().get("workspaceId").is_some_and(|id| {
            id.as_str()
                .is_none_or(|id| !id.eq_ignore_ascii_case(&workspace_id))
        })
    {
        return Err(QueryResolutionError::OwnerMismatch);
    }
    let owner_type = metadata
        .item_type()
        .ok_or(QueryResolutionError::OwnerMismatch)?;
    if item_type.as_deref().is_some_and(|kind| kind != owner_type) {
        return Err(QueryResolutionError::OwnerMismatch);
    }
    if !matches!(owner_type, "DataAgent" | "KQLDatabase") {
        return Err(QueryResolutionError::UnsupportedOwner);
    }
    Ok(QueryOwner {
        path: path.into(),
        artifact: Artifact {
            workspace: workspace_id,
            item: item_id,
        },
        owner_type: owner_type.into(),
    })
}

/// Construct the native KQL binding from actual typed metadata, not a guessed host.
///
/// `engine` and `version` are the native adapter's advertised identifiers.
/// The explicit public Kusto resource and metadata display-name database mapping
/// are existing SDK choices. No query text, ingestion URI, Fabric audience,
/// default database, or alternate account is substituted.
pub async fn resolve_kql<E>(
    owner: QueryOwner,
    client: &Client,
    executor: E,
    engine: &str,
    version: &str,
    max_bytes: NonZeroUsize,
    canceled: impl Future<Output = ()> + Send,
) -> Result<BoundTarget, QueryResolutionError>
where
    E: OperationExecutor,
{
    if owner.owner_type != "KQLDatabase" {
        return Err(QueryResolutionError::UnsupportedOwner);
    }

    /// Verify one configured datasource against the real agent's exact-stage metadata.
    ///
    /// Only decoded tokens continue the same bounded collection; arbitrary returned
    /// URIs are never followed. The complete bounded traversal must establish one
    /// unambiguous record. Missing or unsupported references do not become fictitious
    /// artifacts, alternate stages, or native-provider fallbacks.
    pub async fn resolve_data_agent(
        owner: QueryOwner,
        connection: &QueryConnection,
        client: &Client,
        executor: &dyn OperationExecutor,
        max_bytes: NonZeroUsize,
        canceled: impl Future<Output = ()> + Send,
    ) -> Result<BoundTarget, QueryResolutionError> {
        if owner.owner_type != "DataAgent" {
            return Err(QueryResolutionError::UnsupportedOwner);
        }
        let configured = connection.agent(&owner.artifact)?;
        let stage = match configured.stage() {
            Stage::Production => MetadataStage::Published,
            Stage::Sandbox => MetadataStage::Staging,
        };
        let target =
            DataAgentTarget::new(&owner.artifact.workspace, &owner.artifact.item, "DataAgent")
                .map_err(|error| QueryResolutionError::DataAgent(Box::new(error)))?;
        let mut canceled = pin!(canceled);
        let mut continuation: Option<ContinuationToken> = None;
        let mut seen_tokens = BTreeSet::new();
        let mut seen_ids = BTreeSet::new();
        let mut bytes_remaining = max_bytes.get();
        let mut selected: Option<DatasourceMetadata> = None;
        for _ in 0..8 {
            let bound = NonZeroUsize::new(bytes_remaining)
                .ok_or(QueryResolutionError::IncompleteDatasources)?;
            let page = DataAgentRequests::datasources_page(&target, stage, continuation.as_ref())
                .map_err(|error| QueryResolutionError::DataAgent(Box::new(error)))?
                .execute(
                    client,
                    executor,
                    RepresentationLimits {
                        max_response_bytes: bound,
                        max_result_bytes: bound,
                    },
                    canceled.as_mut(),
                )
                .await
                .map_err(|error| QueryResolutionError::DataAgent(Box::new(error)))?;
            let complete = page.is_complete();
            let charged = serde_json::to_vec(&page)
                .map_err(|_| QueryResolutionError::IncompleteDatasources)?
                .len();
            bytes_remaining = bytes_remaining
                .checked_sub(charged)
                .ok_or(QueryResolutionError::IncompleteDatasources)?;
            for datasource in page.page.value {
                if seen_ids.len() >= 1_000 || !seen_ids.insert(datasource.id().to_ascii_lowercase())
                {
                    return Err(QueryResolutionError::IncompleteDatasources);
                }
                if datasource
                    .id()
                    .eq_ignore_ascii_case(configured.datasource())
                {
                    if selected.is_some() {
                        return Err(QueryResolutionError::IncompleteDatasources);
                    }
                    selected = Some(datasource);
                }
            }
            if complete {
                let datasource = selected.ok_or(QueryResolutionError::UnsupportedDatasource)?;
                let artifact = datasource_artifact(&datasource)?;
                return Ok(BoundTarget {
                    path: owner.path,
                    target: QueryTarget {
                        requested_artifact: owner.artifact.clone(),
                        execution_artifact: artifact.clone(),
                        data_agent: Some(DataAgentRoute {
                            engine: None,
                            agent: owner.artifact,
                            datasource: datasource.id().into(),
                            bound_artifact: artifact,
                            stage: configured.stage(),
                        }),
                        native_kql: Vec::new(),
                    },
                });
            }
            let token = page
                .next_token
                .ok_or(QueryResolutionError::IncompleteDatasources)?;
            if !seen_tokens.insert(token.as_str().to_owned()) {
                return Err(QueryResolutionError::IncompleteDatasources);
            }
            continuation = Some(token);
        }
        Err(QueryResolutionError::IncompleteDatasources)
    }

    fn datasource_artifact(
        datasource: &DatasourceMetadata,
    ) -> Result<Artifact, QueryResolutionError> {
        use serde_json::Value;
        let data = datasource.as_value();
        let reference = match (data.get("itemReference"), data.get("lakehouseReference")) {
            (Some(Value::Object(reference)), None | Some(Value::Null))
            | (None | Some(Value::Null), Some(Value::Object(reference))) => reference,
            _ => return Err(QueryResolutionError::UnsupportedDatasource),
        };
        let workspace = reference
            .get("workspaceId")
            .and_then(Value::as_str)
            .ok_or(QueryResolutionError::UnsupportedDatasource)?;
        let item = reference
            .get("itemId")
            .and_then(Value::as_str)
            .ok_or(QueryResolutionError::UnsupportedDatasource)?;
        let identity = ItemIdentity::new(workspace, item)
            .map_err(|_| QueryResolutionError::UnsupportedDatasource)?;
        if [workspace, item].contains(&"00000000-0000-0000-0000-000000000000") {
            return Err(QueryResolutionError::UnsupportedDatasource);
        }
        Ok(Artifact {
            workspace: identity.workspace_id().into(),
            item: identity.item_id().into(),
        })
    }
    let identity = ItemIdentity::new(&owner.artifact.workspace, &owner.artifact.item)
        .map_err(|_| QueryResolutionError::OwnerMismatch)?;
    let target = ResolvedTarget::new(
        &owner.path,
        identity,
        Target {
            kind: TargetKind::Item,
            owner_type: "KQLDatabase".into(),
            facet: "item".into(),
            representation: "metadata".into(),
            generation: None,
        },
        None,
    )
    .map_err(|_| QueryResolutionError::OwnerMismatch)?;
    let metadata = KqlDatabaseRequests::get(&target, client.context())
        .map_err(|error| QueryResolutionError::Kql(Box::new(error)))?
        .execute(
            executor,
            OperationLimits {
                max_requests: NonZeroUsize::new(1).expect("positive finite request limit"),
                max_response_bytes: max_bytes,
                max_poll_delay: Duration::ZERO,
            },
            canceled,
        )
        .await
        .map_err(|error| QueryResolutionError::Kql(Box::new(error)))?;
    let context = metadata
        .kusto_context(
            DatabaseSelector::MetadataDisplayName,
            KustoResource::PublicService,
        )
        .map_err(|error| QueryResolutionError::Kql(Box::new(error)))?;
    let binding = EngineBinding::kusto(
        engine,
        version,
        context.identity().clone(),
        context.query_endpoint().base().clone(),
        context.database(),
        context.scope(),
        context.metadata_context().selection().account(),
        context.metadata_context().selection().tenant(),
    )
    .map_err(|error| QueryResolutionError::Binding(Box::new(error)))?;
    Ok(BoundTarget {
        path: owner.path,
        target: QueryTarget {
            requested_artifact: owner.artifact.clone(),
            execution_artifact: owner.artifact,
            data_agent: None,
            native_kql: vec![KqlRoute {
                engine: engine.into(),
                binding,
            }],
        },
    })
}
