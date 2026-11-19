// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{KqlRoute, PreparedInput, ResolvedRoute};
use fabric_sdk::runtime::NativeDeadline;
use fabric_sdk_query::{EngineExecutor, KqlQuery, QueryError, QueryLimits, QueryResult, query_kql};
use std::{future::Future, time::Duration};

/// One prepared artifact-native KQL request; no fallback or replay entry point.
pub struct PreparedKql {
    route: KqlRoute,
    query: KqlQuery,
    limits: QueryLimits,
}

/// A wrong provider or an invalid SDK request, rejected without credential I/O.
#[derive(Debug, thiserror::Error)]
pub enum KqlPreparationError {
    /// An AIHub route cannot be dispatched through a Kusto executor.
    #[error("The selected query route is not an artifact-native KQL binding")]
    WrongProvider,
    /// Exact native request or explicit server budget is invalid.
    #[error(transparent)]
    Request(#[from] QueryError),
}

impl PreparedKql {
    /// Bind an opaque statement to one exact native target and finite budgets.
    ///
    /// `server_timeout` is explicit host policy, not a new CLI flag. Production
    /// support is enforced by the SDK executor; this layer never labels network
    /// execution as an offline fixture or weakens native hardline read policy.
    pub fn new(
        route: ResolvedRoute,
        input: PreparedInput,
        request_id: &str,
        server_timeout: Duration,
        limits: QueryLimits,
    ) -> Result<Self, KqlPreparationError> {
        let ResolvedRoute::Kql(route) = route else {
            return Err(KqlPreparationError::WrongProvider);
        };
        let query = KqlQuery::new(input.text(), request_id)?.with_server_timeout(server_timeout)?;
        // Validate the actual serialized request before executor/credential use.
        query.request(&route.binding, limits)?;
        Ok(Self {
            route,
            query,
            limits,
        })
    }

    /// Inspect the exact target without changing it or acquiring credentials.
    pub fn route(&self) -> &KqlRoute {
        &self.route
    }

    /// Consume this request once through the real SDK's guarded streaming path.
    ///
    /// All native datasets and completion/error evidence are returned unchanged.
    /// Permission, timeout, cancellation, warning and partial outcomes never
    /// cause a different engine or a second dispatch.
    pub async fn execute(
        self,
        executor: &dyn EngineExecutor,
        deadline: &NativeDeadline,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<QueryResult, QueryError> {
        query_kql(
            &self.route.binding,
            executor,
            &self.query,
            self.limits,
            deadline,
            cancelled,
        )
        .await
    }
}
