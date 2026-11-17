// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin root-query composition over the real SDK-specific command adapters.
//!
//! The caller resolves the exact parsed path once using its authoritative target
//! snapshot and supplies supported native SDK clients. No provider, credential,
//! endpoint, runtime, clock, file extension, or language is discovered here.
//! Run and Dataflow Execute do not enter this host.

use crate::{
    native_tabular::{NativeCommand, Operation, TextSource},
    tabular_export::{ExportRequest, ExportSink},
};
use fabric_cli_query_commands::{
    AiHubPreparationError, Artifact, InputReader, KqlPreparationError, PreparationError,
    PreparedAiHub, PreparedKql, QueryInput, QueryTarget, ResolvedRoute,
    TextSource as QueryTextSource, prepare_input,
};
use fabric_sdk::runtime::NativeDeadline;
use fabric_sdk_aihub_query::{
    AiHubQueryClient, QueryError as AiHubError, QueryOutcome, TypedQueryResult,
};
use fabric_sdk_query::{EngineExecutor, QueryError as KqlError, QueryLimits, QueryResult};
use std::{future::Future, time::Duration};

/// Optional non-secret query connection bound to the existing trusted service.
#[path = "query_host_connection.rs"]
pub mod connection;

/// Existing SDK metadata and path resolution for supported real query owners.
#[path = "query_host_resolution.rs"]
pub mod resolution;

/// Strict native bridge, rich encoding and standard delivery with retained evidence.
#[path = "query_host_result.rs"]
pub mod result;

/// Real provider construction over the existing selected native dependency stack.
#[cfg(feature = "native-host")]
#[path = "query_host_native.rs"]
pub mod native;

#[cfg(test)]
#[path = "query_host_tests.rs"]
mod tests;

/// One authoritative path resolution, bound to the exact parsed target spelling.
pub struct BoundTarget {
    /// Exact path passed to the target resolver, not a path-suffix heuristic.
    pub path: String,
    /// Requested/execution artifacts and actual supported provider bindings.
    pub target: QueryTarget,
}

/// Existing SDK dependencies, supplied by the supported native host.
///
/// These constructors do not acquire tokens. The selected provider is fixed
/// before its SDK runs; the other provider is never tried after a failure.
pub struct Providers<'a> {
    /// Explicitly qualified AIHub endpoint/audience/context, if configured.
    pub aihub: Option<&'a AiHubQueryClient>,
    /// Exact artifact-native Kusto adapter, if configured.
    pub kql: Option<&'a dyn EngineExecutor>,
}

/// Native host policy, not additional query flags or implicit service defaults.
pub struct QueryPolicy {
    /// Stable host-generated correlation identifier, without query text.
    pub request_id: String,
    /// Explicit production KQL server budget within the shared absolute deadline.
    pub server_timeout: Duration,
    /// Exact native KQL request/response/shape budgets.
    pub kql_limits: QueryLimits,
    /// Explicit AIHub output row cap, not a scan or cost quota.
    pub aihub_max_rows: u32,
}

/// Original authoritative identities retained independently of output delivery.
pub struct QueryIdentity {
    /// Artifact the user actually requested.
    pub requested_artifact: Artifact,
    /// Actual execution artifact, including an authoritative datasource binding.
    pub execution_artifact: Artifact,
    /// Selected advertised engine, absent when a DataAgent advertises no override.
    pub engine: Option<String>,
    /// Exact selected provider binding, retaining DataAgent datasource and stage.
    pub route: ResolvedRoute,
}

/// A real prepared native query, consumed once without another target lookup.
pub struct PreparedQuery<'a> {
    identity: QueryIdentity,
    export: Option<ExportRequest>,
    operation: PreparedProvider<'a>,
}

enum PreparedProvider<'a> {
    AiHub(PreparedAiHub),
    Kql(PreparedKql, &'a dyn EngineExecutor),
}

/// Native provider evidence before strict typed conversion and presentation.
///
/// No legacy CLI table, JSON row-object inference, or silently selected first
/// resultset is introduced. Every dataset and native metadata remains available.
pub enum NativeQueryResult {
    /// Complete explicitly typed AIHub resources and auxiliary evidence.
    AiHub(Box<TypedQueryResult>),
    /// Complete native KQL tables and terminal evidence.
    Kql(Box<QueryResult>),
}

/// Native observation retained independently of success, encoding or delivery.
pub struct QueryObservation {
    /// Actual requested/execution identities and selected engine.
    pub identity: QueryIdentity,
    /// Original explicit format/destination, absent for normal presentation.
    pub export: Option<ExportRequest>,
    /// Every completed dataset or the actual native failure/partial evidence.
    pub result: Result<NativeQueryResult, QueryHostError>,
}

/// A local or native failure; no variant permits another query submission.
#[derive(Debug, thiserror::Error)]
pub enum QueryHostError {
    /// Run/Execute or a target snapshot for another parsed path was supplied.
    #[error("The query host requires the exact resolved root-query target")]
    WrongInvocation,
    /// The chosen supported native provider was not injected.
    #[error("The selected query provider requires an explicitly configured supported native host")]
    NotConfigured,
    /// Native parameter input is rejected before target or credential discovery.
    #[error(transparent)]
    Preparation(#[from] PreparationError),
    /// A format/file request failed standard no-clobber preflight.
    #[error("Query output is unavailable or already exists; no query was dispatched")]
    OutputPreflight,
    /// Actual SDK AIHub request preparation failed before execution.
    #[error(transparent)]
    AiHubPreparation(#[from] AiHubPreparationError),
    /// Actual SDK KQL request preparation failed before execution.
    #[error(transparent)]
    KqlPreparation(#[from] KqlPreparationError),
    /// Real AIHub execution failed with dispatch/uncertainty evidence retained.
    #[error(transparent)]
    AiHub(#[from] AiHubError),
    /// Real KQL execution failed with all partial/native evidence retained.
    #[error(transparent)]
    Kql(#[from] KqlError),
    /// Truncated native data is retained, never published as completed data.
    #[error("AIHub returned truncated data; no completed query file is available")]
    IncompleteAiHub(Box<TypedQueryResult>),
}

/// Prepare the exact parsed root query using one immutable target resolution.
///
/// Standard file preflight precedes any provider dispatch, including no-clobber
/// staging ownership. No export is required for native execution; normal
/// presentation is selected only after complete typed data has been converted.
pub fn prepare<'a>(
    command: &NativeCommand,
    bound: BoundTarget,
    providers: Providers<'a>,
    policy: QueryPolicy,
    input: &mut dyn InputReader,
    output: &mut impl ExportSink,
) -> Result<PreparedQuery<'a>, QueryHostError> {
    let Operation::Query(options) = &command.operation else {
        return Err(QueryHostError::WrongInvocation);
    };
    if command.path != bound.path || command.path.trim().is_empty() {
        return Err(QueryHostError::WrongInvocation);
    }
    let route = bound.target.resolve(options.engine.as_deref())?;
    // Both released native request schemas currently reject parameters.
    // Do this before output staging or opening the statement reader.
    if options.parameters.is_some() {
        return Err(PreparationError::UnsupportedParameters.into());
    }
    match &route {
        ResolvedRoute::AiHub(_) if providers.aihub.is_none() => {
            return Err(QueryHostError::NotConfigured);
        }
        ResolvedRoute::Kql(route) => {
            let executor = providers.kql.ok_or(QueryHostError::NotConfigured)?;
            if executor.binding() != &route.binding {
                return Err(PreparationError::InvalidTarget.into());
            }
        }
        ResolvedRoute::AiHub(_) => {}
    }
    if let Some(request) = &command.export {
        output
            .preflight(request)
            .map_err(|_| QueryHostError::OutputPreflight)?;
    }
    let statement = prepare_input(
        QueryInput {
            text: match &options.text {
                TextSource::Inline(text) => QueryTextSource::Inline(text.clone()),
                TextSource::File(path) => QueryTextSource::File(path.clone()),
            },
            parameters: options.parameters.clone(),
        },
        &route,
        input,
    )?;
    let identity = QueryIdentity {
        requested_artifact: bound.target.requested_artifact,
        execution_artifact: bound.target.execution_artifact,
        engine: route.engine().map(str::to_owned),
        route: route.clone(),
    };
    let operation = match route {
        route @ ResolvedRoute::AiHub(_) => PreparedProvider::AiHub(PreparedAiHub::new(
            route,
            statement,
            providers
                .aihub
                .ok_or(QueryHostError::NotConfigured)?
                .clone(),
            policy.aihub_max_rows,
        )?),
        route @ ResolvedRoute::Kql(_) => PreparedProvider::Kql(
            PreparedKql::new(
                route,
                statement,
                &policy.request_id,
                policy.server_timeout,
                policy.kql_limits,
            )?,
            providers.kql.ok_or(QueryHostError::NotConfigured)?,
        ),
    };
    Ok(PreparedQuery {
        identity,
        export: command.export.clone(),
        operation,
    })
}

/// Truthful root/binary result until a supported native provider is injected.
///
/// The additive Rust feature never invents an endpoint, requests credentials, or
/// promotes a development authentication contract into a production provider.
pub fn not_configured(command: &NativeCommand) -> crate::tabular_export::ExportDelivery {
    let mut delivery = crate::native_tabular::not_configured(command);
    delivery.receipt["delivery"]["error"]["message"] = serde_json::json!(
        "Query requires an explicitly configured supported DataAgent typed or artifact-native KQL provider; no endpoint, audience, identity or datasource is inferred."
    );
    delivery
}

impl PreparedQuery<'_> {
    /// Dispatch the actual prepared SDK request once on the caller's runtime.
    ///
    /// The absolute deadline includes provider credentials and response decoding.
    /// Dropping or canceling local observation never invokes remote stop/replay.
    pub async fn execute(
        self,
        deadline: &NativeDeadline,
        cancelled: impl Future<Output = ()> + Send,
    ) -> QueryObservation {
        let result = async {
            Ok(match self.operation {
                PreparedProvider::AiHub(operation) => {
                    match operation.execute(deadline, cancelled).await? {
                        QueryOutcome::Complete(result) => {
                            NativeQueryResult::AiHub(Box::new(result))
                        }
                        QueryOutcome::Incomplete(result) => {
                            return Err(QueryHostError::IncompleteAiHub(Box::new(result)));
                        }
                    }
                }
                PreparedProvider::Kql(operation, executor) => NativeQueryResult::Kql(Box::new(
                    operation.execute(executor, deadline, cancelled).await?,
                )),
            })
        }
        .await;
        QueryObservation {
            identity: self.identity,
            export: self.export,
            result,
        }
    }
}
