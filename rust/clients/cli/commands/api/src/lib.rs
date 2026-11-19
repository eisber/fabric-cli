// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Raw API grammar, bounded input preparation and caller-bound execution.
//! No runtime, credential provider or HTTP transport is selected here.

mod api;
pub mod parser;

pub use api::{ApiInput, ApiOptions, api, build_api_request, read_api_input};
use fabric_cli_command_contracts::python_truthy;
pub use fabric_cli_command_contracts::{CommandError, CommandValue};
pub use parser::RawApiCommand;

use fabric_sdk_api::{ApiClient, Limits, Origin, RequestBounds};
use std::future::Future;

/// Caller-bound sessions; an absent audience is never inferred or substituted.
pub struct ApiClients<'a> {
    /// Fabric session, also used by the source-compatible Power BI binding.
    pub fabric: &'a ApiClient,
    /// Explicit storage-scoped session.
    pub onelake: Option<&'a ApiClient>,
    /// Explicit ARM-scoped session.
    pub arm: Option<&'a ApiClient>,
    /// Explicit Power BI binding using the source Fabric credential scope.
    pub powerbi: Option<&'a ApiClient>,
}

impl ApiClients<'_> {
    fn get(&self, origin: Origin) -> Result<&ApiClient, CommandError> {
        let client = match origin {
            Origin::Fabric => Some(self.fabric),
            Origin::OneLake => self.onelake,
            Origin::Arm => self.arm,
            Origin::PowerBi => self.powerbi,
            Origin::RelevanceEngine => None,
        };
        client.ok_or_else(|| CommandError::new("NotConfigured", "API audience is not configured"))
    }

    /// Selects the explicit audience before input loading, retaining error order.
    pub async fn execute(
        &self,
        command: &RawApiCommand,
        multipart_boundary: &str,
        limits: Limits,
        request_bounds: RequestBounds,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CommandValue, CommandError> {
        let client = self.get(command.origin)?;
        let input = read_api_input(
            &command.input,
            multipart_boundary,
            request_bounds.max_body_bytes,
        )?;
        api(
            client,
            &ApiOptions {
                origin: command.origin,
                method: command.method,
                endpoint: command.endpoint.clone(),
                params: command.params.clone(),
                headers: command.headers.clone(),
                input,
                query: command.query.clone(),
                show_headers: command.show_headers,
                bounds: request_bounds,
            },
            limits.max_response_bytes,
            cancelled,
        )
        .await
    }
}
