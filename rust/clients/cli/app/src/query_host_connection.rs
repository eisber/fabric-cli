// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Closed non-secret query connection input over the existing saved settings map.

use fabric_cli_query_commands::{Artifact, Stage};
use fabric_sdk::{Audience, EndpointBinding, RequestContext, ServiceOrigin, dataagents::Guid};
use fabric_sdk_aihub_query::AuthBinding;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{collections::BTreeSet, io, io::Write};

const MAX_CONNECTION_BYTES: usize = 64 * 1024;
const MAX_AGENT_BINDINGS: usize = 128;

#[cfg(test)]
#[path = "query_host_connection_tests.rs"]
mod tests;

/// A real configured DataAgent datasource at one explicit stage.
pub struct AgentBinding {
    workspace: String,
    agent: String,
    datasource: String,
    stage: Stage,
}

impl AgentBinding {
    /// Exact configured datasource, still subject to authoritative metadata checks.
    pub fn datasource(&self) -> &str {
        &self.datasource
    }

    /// Explicit configuration stage; no stage fallback is permitted.
    pub const fn stage(&self) -> Stage {
        self.stage
    }
}

/// Validated caller-selected connection, not proof of service authorization.
///
/// The endpoint must match the selected trusted service origin exactly, and the
/// audience must equal that descriptor's supported public-cloud scope. This
/// object contains no account, tenant, credential, token or enablement boolean.
pub struct QueryConnection {
    endpoint: azure_core::http::Url,
    audience_scope: String,
    agents: Vec<AgentBinding>,
}

/// Sanitized connection failures before any token or HTTP operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QueryConnectionError {
    /// Closed schema, protocol, service binding, identifiers or finite bounds failed.
    #[error("Invalid query_connection; use the documented non-secret MCP connection schema")]
    Invalid,
    /// The connection must remain on the existing selected trusted service origin.
    #[error("The query endpoint does not match the selected trusted Fabric service connection")]
    UntrustedEndpoint,
    /// No arbitrary scope can be requested with the selected credential.
    #[error("The query audience does not match the selected trusted service connection")]
    UnsupportedAudience,
    /// One DataAgent cannot resolve to multiple implicit datasource/stage choices.
    #[error("The query connection contains ambiguous DataAgent datasource bindings")]
    AmbiguousAgent,
    /// No fallback agent, datasource, stage or arbitrary bare artifact is selected.
    #[error("The query connection has no exact datasource binding for this DataAgent")]
    MissingAgent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    protocol: String,
    service_binding: String,
    endpoint: String,
    audience_scope: String,
    data_agents: Vec<RawAgent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    workspace_id: String,
    agent_id: String,
    datasource_id: String,
    stage: RawStage,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawStage {
    Published,
    Staging,
}

impl QueryConnection {
    /// Decode only the optional `query_connection` from the existing settings map.
    ///
    /// Missing configuration disables AIHub only, not artifact-native KQL.
    /// This method neither initializes nor writes settings and never acquires
    /// credentials. Unknown fields, including secret/account/tenant overrides,
    /// fail explicitly instead of being ignored.
    pub fn from_settings(
        settings: Option<&Map<String, Value>>,
        trusted: &EndpointBinding,
    ) -> Result<Option<Self>, QueryConnectionError> {
        let Some(value) = settings.and_then(|settings| settings.get("query_connection")) else {
            return Ok(None);
        };
        let mut bounded = BoundedJson(Vec::new());
        serde_json::to_writer(&mut bounded, value).map_err(|_| QueryConnectionError::Invalid)?;
        let raw: RawConnection =
            serde_json::from_slice(&bounded.0).map_err(|_| QueryConnectionError::Invalid)?;
        if raw.protocol != "mcp"
            || raw.service_binding != "dataagent-typed-query"
            || raw.data_agents.is_empty()
            || raw.data_agents.len() > MAX_AGENT_BINDINGS
            || raw.endpoint.len() > 4096
            || raw.audience_scope.len() > 2048
            || raw.endpoint.trim() != raw.endpoint
            || raw.endpoint.contains('\\')
            || raw.endpoint.chars().any(char::is_control)
        {
            return Err(QueryConnectionError::Invalid);
        }
        let endpoint = azure_core::http::Url::parse(&raw.endpoint)
            .map_err(|_| QueryConnectionError::Invalid)?;
        validate_service(trusted, &endpoint, &raw.audience_scope)?;
        let mut seen = BTreeSet::new();
        let mut agents = Vec::with_capacity(raw.data_agents.len());
        for agent in raw.data_agents {
            for id in [&agent.workspace_id, &agent.agent_id, &agent.datasource_id] {
                Guid::new(id.as_str()).map_err(|_| QueryConnectionError::Invalid)?;
            }
            if !seen.insert((
                agent.workspace_id.to_ascii_lowercase(),
                agent.agent_id.to_ascii_lowercase(),
            )) {
                return Err(QueryConnectionError::AmbiguousAgent);
            }
            agents.push(AgentBinding {
                workspace: agent.workspace_id,
                agent: agent.agent_id,
                datasource: agent.datasource_id,
                stage: match agent.stage {
                    RawStage::Published => Stage::Production,
                    RawStage::Staging => Stage::Sandbox,
                },
            });
        }
        Ok(Some(Self {
            endpoint,
            audience_scope: raw.audience_scope,
            agents,
        }))
    }

    /// Select only the exact configured real agent; no implicit first binding.
    pub fn agent(&self, owner: &Artifact) -> Result<&AgentBinding, QueryConnectionError> {
        self.agents
            .iter()
            .find(|agent| {
                agent.workspace.eq_ignore_ascii_case(&owner.workspace)
                    && agent.agent.eq_ignore_ascii_case(&owner.item)
            })
            .ok_or(QueryConnectionError::MissingAgent)
    }

    /// Revalidate the same trusted connection before binding the existing credential.
    ///
    /// The SDK receives only the explicitly selected endpoint/scope and exact
    /// existing account/tenant. A caller configuration is not live endpoint/auth
    /// qualification; 401/403 remain actual failures without provider fallback.
    pub fn auth_binding(
        &self,
        context: &RequestContext,
    ) -> Result<AuthBinding, QueryConnectionError> {
        validate_service(context.endpoint(), &self.endpoint, &self.audience_scope)?;
        if context.selection().audience() != Audience::Fabric {
            return Err(QueryConnectionError::UnsupportedAudience);
        }
        AuthBinding::new(
            self.endpoint.clone(),
            self.audience_scope.clone(),
            context.selection().tenant().into(),
            context.selection().account().into(),
        )
        .map_err(|_| QueryConnectionError::Invalid)
    }
}

fn validate_service(
    trusted: &EndpointBinding,
    endpoint: &azure_core::http::Url,
    scope: &str,
) -> Result<(), QueryConnectionError> {
    if !matches!(
        trusted.origin(),
        ServiceOrigin::Fabric | ServiceOrigin::PowerBi
    ) || endpoint.query().is_some()
        || endpoint.path().contains(['%', '\\'])
    {
        return Err(QueryConnectionError::UntrustedEndpoint);
    }
    trusted
        .validate_url(endpoint)
        .map_err(|_| QueryConnectionError::UntrustedEndpoint)?;
    if scope != trusted.audience().scope() {
        return Err(QueryConnectionError::UnsupportedAudience);
    }
    Ok(())
}

struct BoundedJson(Vec<u8>);

impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > MAX_CONNECTION_BYTES)
        {
            return Err(io::Error::other(
                "Query connection exceeds its finite byte bound",
            ));
        }
        self.0
            .try_reserve_exact(bytes.len())
            .map_err(|_| io::Error::other("Query connection allocation failed"))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
