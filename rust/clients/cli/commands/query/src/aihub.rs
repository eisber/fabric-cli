// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{PreparedInput, ResolvedRoute, Stage};
use fabric_sdk_aihub_query::{
    AiHubQueryClient, DataAgentTarget, NativeDeadline, NativeWorkbookRequest,
    NativeWorkbookResponse, QueryError, QueryOutcome, QueryRequest, QueryStage, TypedJson,
};
use std::future::Future;

/// One prepared real DataAgent datasource call with a caller-configured SDK client.
///
/// The client already contains the explicitly qualified endpoint, audience,
/// credential, HTTP executor and timer. This type discovers no identity or route.
pub struct PreparedAiHub {
    client: AiHubQueryClient,
    request: QueryRequest,
}

/// One explicit workload-native workbook execution, never a typed fallback.
pub struct PreparedAiHubWorkbook {
    client: AiHubQueryClient,
    request: NativeWorkbookRequest,
}

fn target(route: ResolvedRoute) -> Result<DataAgentTarget, AiHubPreparationError> {
    let ResolvedRoute::AiHub(route) = route else {
        return Err(AiHubPreparationError::WrongProvider);
    };
    let stage = match route.stage {
        Stage::Production => QueryStage::Production,
        Stage::Sandbox => QueryStage::Sandbox,
    };
    Ok(DataAgentTarget::new(
        &route.agent.item,
        &route.datasource,
        stage,
    )?)
}

/// An unsupported provider or invalid typed DataAgent request before dispatch.
#[derive(Debug, thiserror::Error)]
pub enum AiHubPreparationError {
    /// Bare artifacts and native KQL bindings cannot become DataAgents.
    #[error("The selected query route is not a supported DataAgent datasource")]
    WrongProvider,
    /// The actual SDK target, statement or explicit row budget is invalid.
    #[error(transparent)]
    Request(#[from] QueryError),
}

impl PreparedAiHub {
    /// Bind one authoritative DataAgent datasource and require typed JSON.
    ///
    /// The native host supplies its finite output row policy. No filename-derived
    /// format, template-language selector, or unsupported native parameters are
    /// synthesized. The SDK sends both exact DataAgent/TypedQueryResults tokens.
    pub fn new(
        route: ResolvedRoute,
        input: PreparedInput,
        client: AiHubQueryClient,
        max_rows: u32,
    ) -> Result<Self, AiHubPreparationError> {
        let target = target(route)?;
        let request = QueryRequest::new(target, input.text().to_owned(), TypedJson::Required)?
            .with_max_rows(max_rows)?;
        Ok(Self { client, request })
    }

    /// Consume once using the caller's existing runtime and absolute deadline.
    ///
    /// Every native typed result and auxiliary resource is retained unchanged.
    /// Missing typed JSON, permission failures, uncertain completion and
    /// truncation do not trigger CSV parsing or another provider.
    pub async fn execute(
        self,
        deadline: &NativeDeadline,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<QueryOutcome, QueryError> {
        self.client
            .execute_query(&self.request, deadline, cancelled)
            .await
    }
}

impl PreparedAiHubWorkbook {
    /// Bind the authoritative datasource without acquiring credentials or I/O.
    pub fn new(
        route: ResolvedRoute,
        input: PreparedInput,
        client: AiHubQueryClient,
        max_rows: u32,
    ) -> Result<Self, AiHubPreparationError> {
        let request = NativeWorkbookRequest::new(target(route)?, input.text().to_owned())?
            .with_max_rows(max_rows)?;
        Ok(Self { client, request })
    }

    /// Execute once, retaining successful-response and attachment-failure evidence.
    pub async fn execute(
        self,
        deadline: &NativeDeadline,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<NativeWorkbookResponse, QueryError> {
        self.client
            .execute_native_workbook(&self.request, deadline, cancelled)
            .await
    }
}
