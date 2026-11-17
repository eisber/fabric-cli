// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::PreparationError;
use fabric_sdk_query::EngineBinding;
use fabric_v2_contracts::ItemIdentity;
use std::fmt;

/// An authoritative Fabric artifact identity; no identity is inferred from text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artifact {
    /// Exact workspace identifier supplied by the target resolver.
    pub workspace: String,
    /// Exact artifact identifier supplied by the target resolver.
    pub item: String,
}

impl Artifact {
    fn valid(&self) -> bool {
        guid(&self.workspace) && guid(&self.item)
    }

    fn matches(&self, owner: &ItemIdentity) -> bool {
        self.workspace.eq_ignore_ascii_case(owner.workspace_id())
            && self.item.eq_ignore_ascii_case(owner.item_id())
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.workspace.eq_ignore_ascii_case(&other.workspace)
            && self.item.eq_ignore_ascii_case(&other.item)
    }
}

/// Explicit DataAgent stage supplied by authoritative target resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    /// Published DataAgent configuration.
    Production,
    /// Explicit draft DataAgent configuration.
    Sandbox,
}

/// An actual resolved DataAgent datasource, not a fabricated wrapper artifact.
#[derive(Clone)]
pub struct DataAgentRoute {
    /// Exact advertised engine/capability identifier.
    pub engine: String,
    /// DataAgent whose configuration owns this datasource.
    pub agent: Artifact,
    /// Exact datasource identifier within that DataAgent.
    pub datasource: String,
    /// Actual artifact bound to the datasource.
    pub bound_artifact: Artifact,
    /// Explicit stage, independent of typed-result opt-in.
    pub stage: Stage,
}

impl fmt::Debug for DataAgentRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DataAgentRoute(<explicit>)")
    }
}

/// An exact artifact-native Kusto binding, not a language-text heuristic.
#[derive(Clone)]
pub struct KqlRoute {
    /// Exact advertised engine/capability identifier.
    pub engine: String,
    /// Resolved native endpoint/database/audience/account/tenant and Fabric owner.
    pub binding: EngineBinding,
}

impl fmt::Debug for KqlRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KqlRoute(<explicit>)")
    }
}

/// One immutable authoritative target snapshot, resolved by the native host.
///
/// Supply only positively supported read-only bindings. Endpoint/auth qualification
/// remains the caller's responsibility; this does not promote a development token
/// flow or claim universal AIHub coverage for direct Report/SemanticModel artifacts.
pub struct QueryTarget {
    /// Artifact explicitly requested by the caller.
    pub requested_artifact: Artifact,
    /// Actual execution artifact, including an authoritative datasource binding.
    pub execution_artifact: Artifact,
    /// Supported DataAgent typed route, if positively configured.
    pub data_agent: Option<DataAgentRoute>,
    /// Every advertised artifact-native KQL binding.
    pub native_kql: Vec<KqlRoute>,
}

/// Exactly one selected provider; execution never falls back to another variant.
#[derive(Clone, Debug)]
pub enum ResolvedRoute {
    /// AIHub with both exact DataAgent routing and typed-result variants.
    AiHub(DataAgentRoute),
    /// Artifact-native KQL with strict native read enforcement.
    Kql(KqlRoute),
}

impl ResolvedRoute {
    /// Native request limit, not a scan, cost or allocator-wide memory quota.
    pub const fn max_statement_bytes(&self) -> usize {
        match self {
            Self::AiHub(_) => 1024 * 1024,
            Self::Kql(_) => 4 * 1024 * 1024,
        }
    }

    /// Selected advertised engine ID, without inventing public grammar.
    pub fn engine(&self) -> &str {
        match self {
            Self::AiHub(route) => &route.engine,
            Self::Kql(route) => &route.engine,
        }
    }
}

impl QueryTarget {
    /// Bind once before input, credentials and dispatch, honoring explicit engines.
    ///
    /// A supported DataAgent datasource uses AIHub by default. Bare artifacts do
    /// not become fictitious DataAgents. An explicit engine must match exactly
    /// one advertised route; absent/ambiguous native bindings fail closed.
    pub fn resolve(&self, engine: Option<&str>) -> Result<ResolvedRoute, PreparationError> {
        if !self.requested_artifact.valid()
            || !self.execution_artifact.valid()
            || engine.is_some_and(|engine| !identifier(engine))
        {
            return Err(PreparationError::InvalidTarget);
        }
        if let Some(route) = &self.data_agent
            && (!identifier(&route.engine)
                || !route.agent.valid()
                || !guid(&route.datasource)
                || !route.bound_artifact.same_identity(&self.execution_artifact)
                || !route.agent.same_identity(&self.requested_artifact))
        {
            return Err(PreparationError::InvalidTarget);
        }
        if self.native_kql.iter().any(|route| {
            !identifier(&route.engine) || !self.execution_artifact.matches(route.binding.owner())
        }) {
            return Err(PreparationError::InvalidTarget);
        }
        let mut routes = self
            .data_agent
            .iter()
            .cloned()
            .map(ResolvedRoute::AiHub)
            .chain(self.native_kql.iter().cloned().map(ResolvedRoute::Kql))
            .filter(|route| engine.is_none_or(|engine| route.engine() == engine));
        if engine.is_none()
            && let Some(route) = &self.data_agent
        {
            return Ok(ResolvedRoute::AiHub(route.clone()));
        }
        let selected = routes.next().ok_or(if engine.is_some() {
            PreparationError::UnsupportedEngine
        } else {
            PreparationError::MissingProvider
        })?;
        if routes.next().is_some() {
            return Err(PreparationError::AmbiguousProvider);
        }
        Ok(selected)
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value != "00000000-0000-0000-0000-000000000000"
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
