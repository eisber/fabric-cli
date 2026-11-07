// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-shaped read projections, not generic authoring documents.

use crate::scope::Guid;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Supported explicit reference discriminator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReferenceType {
    /// Referenced by item/workspace GUID, not display name.
    ById,
}

/// Ontology/physical item reference, distinct from a virtual source's identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemReference {
    /// Source discriminator; never silently defaulted on malformed responses.
    pub reference_type: ReferenceType,
    /// Referenced item, not the DataAgent.
    pub item_id: Guid,
    /// Referenced workspace, not necessarily the agent's workspace.
    pub workspace_id: Guid,
}

impl ItemReference {
    /// Creates an explicit ById reference.
    pub fn by_id(item_id: Guid, workspace_id: Guid) -> Self {
        Self {
            reference_type: ReferenceType::ById,
            item_id,
            workspace_id,
        }
    }
}

/// Attachment list/member response; entity selection lives on `/elements`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OntologyAttachment {
    /// Ontology item ID returned by the service.
    pub id: Guid,
    /// May be absent on degraded read projections.
    pub ontology_reference: Option<ItemReference>,
    /// User-facing display name.
    pub display_name: Option<String>,
    /// Source-wide live-access failure state, not absence.
    pub state: Option<String>,
    /// Uninterpreted forward-compatible service metadata.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Strict page envelope: absent/malformed `value` cannot become empty success.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// Ordered source rows.
    pub value: Vec<T>,
    /// Opaque continuation token.
    pub continuation_token: Option<String>,
    /// Advertised URI metadata; never automatically followed.
    pub continuation_uri: Option<String>,
}

/// Returned navigation/selection element with opaque ID.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    /// Returned ID used unchanged by selection and drill-down.
    pub id: String,
    /// Effective entity-derived selection; not permission to select a source column.
    pub is_selected: bool,
    /// Display name, never an identity substitute.
    pub display_name: String,
    /// Native element discriminator; grouping nodes are not selectable entities.
    #[serde(rename = "type")]
    pub kind: String,
    /// Whether navigation children exist.
    pub has_sub_elements: bool,
    /// Degraded merge/access state.
    pub state: Option<String>,
    /// Optional source roll-up; preserved rather than recomputed.
    pub selection_state: Option<String>,
    /// Description/data type/ancestors and other native metadata.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Virtual source projection, filtered by selected entities and caller access.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OntologySource {
    /// Returned source identity; not necessarily its enclosing physical item ID.
    pub id: Guid,
    /// Absent on some retained unavailable-source tombstones.
    pub workspace_id: Option<Guid>,
    /// Physical/queryable reference, when available.
    pub item_reference: Option<ItemReference>,
    /// Available or retained display name.
    pub display_name: Option<String>,
    /// Binding type can remain Eventhouse while `id` is the child KQL database.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Includes PendingConversion/NotAvailable; never treated as empty success.
    pub state: Option<String>,
    /// Merged authored instructions, not generated context markdown.
    pub instructions: Option<String>,
    /// Merged authored description.
    pub description: Option<String>,
    /// Merged authored examples; reading them never executes their queries.
    pub example_queries: Option<Vec<FewShot>>,
    /// Resource-scoped validator for all retained custom setup.
    pub e_tag: Option<String>,
    /// Warnings/selection state and forward-compatible fields.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Authored configuration uses persisted names unlike the request DTO.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInstructions {
    /// Returned durable source ID.
    pub artifact_id: Guid,
    /// Optional identity snapshot.
    pub workspace_id: Option<Guid>,
    /// Authored instruction source.
    pub data_source_instructions: Option<String>,
    /// Authored description source.
    pub user_description: Option<String>,
    /// Schema/display/type snapshots and forward-compatible metadata.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Served selected-source raw schema, never an authoring document.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSchema {
    /// Returned source ID checked against the requested source.
    pub data_source_id: Guid,
    /// Cache write time from the source; not invented as "now".
    pub cache_last_updated_time: String,
    /// Native DataSourceElement tree, not management Element response DTOs.
    pub elements: Vec<SchemaElement>,
}

/// Native cached schema node; its snake-case contract differs from management elements.
#[derive(Clone, Debug, Deserialize)]
pub struct SchemaElement {
    /// Optional source-native identity, not a management mutation ID.
    pub id: Option<String>,
    /// Effective entity-derived selection, never an editable source-column selector.
    pub is_selected: bool,
    /// Native `display_name`, not management `displayName`.
    pub display_name: String,
    /// Native numeric/string enum representation, preserved without ID coercion.
    #[serde(rename = "type")]
    pub kind: Value,
    /// Recursive native schema nodes.
    pub children: Vec<SchemaElement>,
    /// Source descriptions/types/definitions and forward-compatible metadata.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Stored authored example with native validation evidence.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FewShot {
    /// Server-generated example identity, never inferred from its question.
    pub id: Guid,
    /// Authored question.
    pub question: String,
    /// Authored query, not executed by this package.
    pub query: Option<String>,
    /// Native validation status and diagnostics, preserved without reinterpretation.
    pub validation_status: Map<String, Value>,
}

/// Per-entity selection result preserves failures inside HTTP 200.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySelectionStatus {
    /// Echoed opaque entity ID.
    pub element_id: String,
    /// Native outcome; NotFound is not silently upgraded to success.
    pub status: String,
}

/// Ordered outcome from native bulk entity selection.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionResult {
    /// One status per explicit ID, or per top-level entity for apply-all.
    pub elements_status: Vec<EntitySelectionStatus>,
}

/// Completed explicit whole-agent publication projection.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    /// Optional published description.
    #[serde(deserialize_with = "required_nullable")]
    pub published_description: Option<String>,
    /// Verified source projection statuses; no client-manufactured success.
    pub topic_projections: Option<Vec<Map<String, Value>>>,
    /// Forward-compatible service metadata.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Generated staging context markdown, a served projection with no write conversion.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(transparent)]
pub struct ContextMarkdown {
    /// Selected-entity context assembled by the server; empty text is valid.
    pub markdown: String,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}
