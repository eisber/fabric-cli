// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::RelevanceError;
use azure_core::http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, fmt, str::FromStr};

/// Service limit, measured as .NET UTF-16 code units, not UTF-8 bytes.
pub const MAX_SEARCH_TEXT_UTF16: usize = 500;
/// This SDK's deliberately smaller top-k bound; the service permits up to 200.
pub const MAX_RESULTS: usize = 50;
/// Exact verified `RelevanceEngineServiceRoutes.Prefix`, without leading slash.
pub const RELEVANCE_ENGINE_PREFIX: &str = "relevanceengine/v1";
/// Exact verified action, relative to the reviewed Relevance Engine root.
pub const DISCOVER_ARTIFACTS_ACTION: &str = "discoverArtifacts";
/// The pinned controller route, independent of any hostname or token audience.
pub const DISCOVER_ARTIFACTS_PATH: &str = "/relevanceengine/v1/discoverArtifacts";

/// Explicit tenant or exact workspace-ID scope; no implicit name matching.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryScope {
    /// Discover accessible artifacts across the explicitly selected tenant.
    Tenant,
    /// An exact workspace selector, sent as a scoped `artifactObjectIds` entry.
    Workspace {
        /// Opaque caller-selected workspace ID; never used as a workspace name.
        id: String,
    },
}

/// Verified Relevance Engine artifact kinds.
///
/// These are registry IDs, not Fabric REST item type IDs. Paginated reports and
/// workspace documents can appear as related metadata, but are deliberately
/// outside this provider's discoverable request subset.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ArtifactType {
    /// A semantic model, registry ID 2.
    SemanticModel,
    /// A Power BI report, registry ID 3.
    Report,
    /// A data agent, called AISkill by the discovery contract, registry ID 5003.
    AISkill,
    /// An organizational app, registry ID 16000.
    OrgApp,
    /// An app, registry ID 16001.
    App,
    /// An ontology, registry ID 21005.
    Ontology,
    /// Related paginated-report metadata; not an accepted request filter.
    RdlReport,
    /// Related workspace metadata; not an accepted request filter.
    Workspace,
}

impl ArtifactType {
    /// Complete supported request subset; no unspecified or unknown type exists.
    pub const ALL: [Self; 6] = [
        Self::SemanticModel,
        Self::Report,
        Self::AISkill,
        Self::OrgApp,
        Self::App,
        Self::Ontology,
    ];

    /// Numeric value required in `discoveryScope.allowedArtifactTypes`.
    pub const fn registry_id(self) -> u32 {
        match self {
            Self::SemanticModel => 2,
            Self::Report => 3,
            Self::AISkill => 5003,
            Self::OrgApp => 16000,
            Self::App => 16001,
            Self::Ontology => 21005,
            Self::RdlReport => 201,
            Self::Workspace => 17000,
        }
    }

    /// Exact service spelling used in the response's `documentType`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SemanticModel => "SemanticModel",
            Self::Report => "Report",
            Self::AISkill => "AISkill",
            Self::OrgApp => "OrgApp",
            Self::App => "App",
            Self::Ontology => "Ontology",
            Self::RdlReport => "RdlReport",
            Self::Workspace => "Workspace",
        }
    }
}

impl FromStr for ArtifactType {
    type Err = RelevanceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or(RelevanceError::UnsupportedType)
    }
}

/// Validated direct discovery input. No query rewriting or ambient scope exists.
#[derive(Clone, Eq, PartialEq)]
pub struct DiscoveryRequest {
    text: String,
    max_results: usize,
    scope: DiscoveryScope,
    artifact_types: Vec<ArtifactType>,
}

impl DiscoveryRequest {
    /// Validates input before any credential or executor can be accessed.
    ///
    /// Text is preserved exactly, including internal/leading/trailing whitespace.
    /// An empty type list is rejected because the service would expand it.
    /// Duplicate types are rejected rather than silently normalizing the input.
    pub fn new(
        text: impl Into<String>,
        max_results: usize,
        scope: DiscoveryScope,
        artifact_types: Vec<ArtifactType>,
    ) -> Result<Self, RelevanceError> {
        let scope = match scope {
            DiscoveryScope::Tenant => DiscoveryScope::Tenant,
            DiscoveryScope::Workspace { id } => {
                let id = canonical_guid(&id)
                    .filter(|id| id != "00000000-0000-0000-0000-000000000000")
                    .ok_or(RelevanceError::InvalidWorkspaceId)?;
                DiscoveryScope::Workspace { id }
            }
        };
        let text = text.into();
        if text.trim().is_empty() || text.encode_utf16().count() > MAX_SEARCH_TEXT_UTF16 {
            return Err(RelevanceError::InvalidText);
        }
        if !(1..=MAX_RESULTS).contains(&max_results) {
            return Err(RelevanceError::InvalidMaxResults);
        }
        let unique: HashSet<_> = artifact_types.iter().copied().collect();
        if artifact_types.is_empty() || unique.len() != artifact_types.len() {
            return Err(RelevanceError::InvalidTypes);
        }
        if artifact_types
            .iter()
            .any(|kind| !ArtifactType::ALL.contains(kind))
        {
            return Err(RelevanceError::UnsupportedType);
        }
        Ok(Self {
            text,
            max_results,
            scope,
            artifact_types,
        })
    }

    /// Original nonblank search text, never an LLM-generated replacement.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Explicit finite top-k, in the range 1 through 50.
    pub const fn max_results(&self) -> usize {
        self.max_results
    }

    /// Exact allowed kinds, in caller order.
    pub fn artifact_types(&self) -> &[ArtifactType] {
        &self.artifact_types
    }

    /// Validated selected scope, preserving exact workspace identity.
    pub fn scope(&self) -> &DiscoveryScope {
        &self.scope
    }

    /// Direct discovery is a read-only POST, never an implicitly replayable GET.
    pub const fn method(&self) -> Method {
        Method::Post
    }

    /// Pinned controller path, to be bound to the reviewed shared service origin.
    pub const fn path(&self) -> &'static str {
        DISCOVER_ARTIFACTS_PATH
    }

    /// Action relative to a shared origin already rooted at `/relevanceengine/v1/`.
    pub const fn action(&self) -> &'static str {
        DISCOVER_ARTIFACTS_ACTION
    }

    /// Exact URI query pairs, for encoding once by the shared request builder.
    pub fn query(&self) -> Vec<(String, String)> {
        vec![("searchText".to_owned(), self.text.clone())]
    }

    /// Exact verified POST body with fixed SearchQuery intent (`search-intent`),
    /// excluding experimentation and Copilot options.
    pub fn body(&self) -> Value {
        let mut discovery_scope = json!({
            "queryScope": match self.scope {
                DiscoveryScope::Tenant => "Global",
                DiscoveryScope::Workspace { .. } => "Workspace",
            },
            "allowedArtifactTypes": self.artifact_types.iter()
                .map(|kind| kind.registry_id()).collect::<Vec<_>>(),
        });
        if let DiscoveryScope::Workspace { id } = &self.scope {
            discovery_scope["artifactObjectIds"] = json!([id]);
        }
        json!({
            "discoveryScope": discovery_scope,
            "top": self.max_results,
            "userIntent": "search-intent",
        })
    }
}

impl fmt::Debug for DiscoveryRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscoveryRequest")
            .field("text", &"<redacted>")
            .field("max_results", &self.max_results)
            .field(
                "scope",
                &match self.scope {
                    DiscoveryScope::Tenant => "Tenant",
                    DiscoveryScope::Workspace { .. } => "Workspace(<redacted>)",
                },
            )
            .field("artifact_types", &self.artifact_types)
            .finish()
    }
}

pub(crate) fn canonical_guid(value: &str) -> Option<String> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}
