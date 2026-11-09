// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    ArtifactType, DiscoveryRequest, DiscoveryScope, RelevanceError, request::canonical_guid,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error};
use serde_json::{Map, Value};

/// Relevance confidence encoded as a numeric enum by the pinned public DTO.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelevanceConfidence {
    /// Service value 0.
    Irrelevant,
    /// Service value 1.
    BarelyRelevant,
    /// Service value 2.
    SomewhatRelevant,
    /// Service value 3.
    HighlyRelevant,
    /// Service value 4.
    ExtremelyRelevant,
}

impl<'de> Deserialize<'de> for RelevanceConfidence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u8::deserialize(deserializer)? {
            0 => Ok(Self::Irrelevant),
            1 => Ok(Self::BarelyRelevant),
            2 => Ok(Self::SomewhatRelevant),
            3 => Ok(Self::HighlyRelevant),
            4 => Ok(Self::ExtremelyRelevant),
            _ => Err(D::Error::custom("unknown relevance confidence")),
        }
    }
}

// Required C# reference/nullable fields must be present, but may be JSON null.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Known public artifact fields with unrecognized optional properties preserved.
///
/// Nullable required reference fields retain null rather than fabricating an ID,
/// name, URL or empty matched-signal list. Artifact URLs are metadata only; this
/// SDK never follows them or sends credentials to them.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Verified document kind (string on the response wire).
    pub document_type: ArtifactType,
    /// Name; required property, but a nullable C# string.
    #[serde(deserialize_with = "required_nullable")]
    pub display_name: Option<String>,
    /// Service ranking score, not a cross-provider normalized score.
    pub score: f64,
    /// Required nullable Azure search score.
    #[serde(deserialize_with = "required_nullable")]
    pub search_score: Option<f64>,
    /// Required nullable matched-property list.
    #[serde(deserialize_with = "required_nullable")]
    pub matched_properties: Option<Vec<String>>,
    /// Required nullable matched-signal list.
    #[serde(deserialize_with = "required_nullable")]
    pub matched_signals: Option<Vec<String>>,
    /// Required nullable metadata URL, not an execution target.
    #[serde(deserialize_with = "required_nullable")]
    pub url: Option<String>,
    /// Required nullable artifact identifier, preserved as the service string.
    #[serde(deserialize_with = "required_nullable")]
    pub object_id: Option<String>,
    /// Required nullable workspace identifier.
    #[serde(deserialize_with = "required_nullable")]
    pub workspace_id: Option<String>,
    /// Required nullable workspace name.
    #[serde(deserialize_with = "required_nullable")]
    pub workspace_name: Option<String>,
    /// Required numeric relevance enum.
    pub relevance: RelevanceConfidence,
    /// Optional description.
    pub description: Option<String>,
    /// Optional endorsement.
    pub endorsement: Option<String>,
    /// Optional semantic reranker score.
    pub semantic_reranker_score: Option<f64>,
    /// Optional text-only search score.
    pub search_text_score: Option<f64>,
    /// Optional per-vector ranking metadata.
    pub search_vector_scores: Option<Vec<VectorScore>>,
    /// Optional related artifacts, decoded with the same strict field types.
    pub related_to_artifacts: Option<Vec<Artifact>>,
    /// Optional Copilot preparation metadata, not permission to invoke Copilot.
    pub is_prepared_for_copilot: Option<bool>,
    /// Optional Copilot reader-restriction metadata.
    pub restrict_copilot_for_readers: Option<bool>,
    /// Optional opening timestamp, preserved as a service string.
    pub last_opened_in_utc: Option<String>,
    /// Optional matched answer prompts, not instructions or an invocation request.
    pub matched_verified_answers_prompts: Option<Vec<String>>,
    /// Optional short caption.
    pub short_caption: Option<String>,
    /// Optional long caption.
    pub long_caption: Option<String>,
    /// Optional sensitivity label metadata.
    pub sensitivity_label_id: Option<String>,
    /// Other optional properties, including diagnostics added by newer services.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// One verified optional Azure search vector-score record.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VectorScore {
    /// Index of the service's vector query.
    pub query_index: i32,
    /// Required nullable search field name.
    #[serde(deserialize_with = "required_nullable")]
    pub field_name: Option<String>,
    /// Optional search score.
    pub search_score: Option<f64>,
    /// Optional vector similarity.
    pub vector_similarity: Option<f64>,
    /// Optional search score provenance.
    pub search_score_source: Option<ScoreSource>,
    /// Optional similarity provenance.
    pub vector_similarity_source: Option<ScoreSource>,
    /// Forward-compatible optional metadata.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Verified string enum used by optional vector score provenance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub enum ScoreSource {
    /// Value was returned by Azure search.
    ReturnedByAzure,
    /// Value was not returned by Azure search.
    NotReturned,
}

/// Known optional diagnostics. Metadata is never interpreted as an instruction.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryDiagnostics {
    /// Whether the service generated a vector for the search text.
    pub is_vector_embedding_generated_for_search_query: Option<bool>,
    /// Optional service search type label.
    pub search_type: Option<String>,
    /// Optional semantic ranking indicator.
    pub semantic_ranking: Option<bool>,
    /// Optional maximum Azure score.
    pub max_azure_score: Option<f64>,
    /// Optional query embedding dimensions.
    pub query_embedding_dimensions: Option<i32>,
    /// Optional untrusted service explanation.
    pub embedding_failure_reason: Option<String>,
    /// Optional indication of whether score diagnostics were requested.
    pub score_debug_requested: Option<bool>,
    /// Optional indication of whether score diagnostics reached Azure.
    pub score_debug_sent_to_azure: Option<bool>,
    /// Unrecognized optional diagnostics.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Coverage is ranked, bounded and non-exhaustive even when zero rows are found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryCoverage {
    /// One relevance-ranked top-k response; no total or continuation is asserted.
    RankedTopK,
    /// Empty query ID identifies the disabled/unknown controller response.
    DisabledOrUnknown,
}

/// One complete decoded HTTP response, not an exhaustive resource inventory.
#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveryResult {
    /// Canonical service query identifier; may be all zero for disabled discovery.
    pub query_id: String,
    /// Rows in exact service rank order, including duplicates.
    pub artifacts: Vec<Artifact>,
    /// Explicit coverage, never authoritative-zero or exhaustive.
    pub coverage: DiscoveryCoverage,
    /// Optional diagnostics as untrusted service metadata.
    pub diagnostics: Option<DiscoveryDiagnostics>,
    /// Unrecognized optional response fields, retained without interpreting them.
    pub additional: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireResult {
    query_id: String,
    artifacts: Vec<Artifact>,
    diagnostics: Option<DiscoveryDiagnostics>,
    #[serde(flatten)]
    additional: Map<String, Value>,
}

pub(crate) fn decode(
    bytes: &[u8],
    request: &DiscoveryRequest,
) -> Result<DiscoveryResult, RelevanceError> {
    let wire: WireResult =
        serde_json::from_slice(bytes).map_err(|_| RelevanceError::InvalidResponse)?;
    let query_id = canonical_guid(&wire.query_id).ok_or(RelevanceError::InvalidResponse)?;
    if wire.artifacts.len() > request.max_results()
        || wire
            .artifacts
            .iter()
            .any(|artifact| !request.artifact_types().contains(&artifact.document_type))
        || matches!(
            request.scope(),
            DiscoveryScope::Workspace { id }
                if wire.artifacts.iter().any(|artifact|
                    artifact.workspace_id.as_ref()
                        .is_none_or(|workspace| !workspace.eq_ignore_ascii_case(id))
                )
        )
    {
        return Err(RelevanceError::InvalidResponse);
    }
    let coverage = if query_id == "00000000-0000-0000-0000-000000000000" {
        if !wire.artifacts.is_empty() {
            return Err(RelevanceError::InvalidResponse);
        }
        DiscoveryCoverage::DisabledOrUnknown
    } else {
        DiscoveryCoverage::RankedTopK
    };
    Ok(DiscoveryResult {
        query_id,
        artifacts: wire.artifacts,
        coverage,
        diagnostics: wire.diagnostics,
        additional: wire.additional,
    })
}
