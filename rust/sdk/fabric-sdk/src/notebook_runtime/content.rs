// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, lossless JSON notebook snapshots and whole-document transformations.

use crate::runtime::NativeError;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{io, sync::Arc};

/// Finite budgets applied on import and after every local transformation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentLimits {
    /// Maximum encoded JSON bytes, including unknown fields and attachments.
    pub document_bytes: usize,
    /// Maximum number of cells.
    pub cells: usize,
    /// Maximum UTF-8 bytes of one cell's joined source.
    pub source_bytes: usize,
    /// Maximum saved output records per cell.
    pub outputs_per_cell: usize,
}

impl Default for ContentLimits {
    fn default() -> Self {
        Self {
            document_bytes: 16 * 1024 * 1024,
            cells: 10_000,
            source_bytes: 1024 * 1024,
            outputs_per_cell: 10_000,
        }
    }
}

impl ContentLimits {
    pub(super) fn validate(self) -> Result<(), NativeError> {
        if self.document_bytes == 0
            || self.cells == 0
            || self.source_bytes == 0
            || self.outputs_per_cell == 0
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(())
    }
}

/// Explicit opt-in to a provider write with no advertised `If-Match` guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritePolicy {
    /// Permit an unguarded full notebook PUT, potentially overwriting concurrent edits.
    AllowUnguarded,
    /// Require remote compare-and-swap; unsupported by this source-pinned endpoint.
    RequireGuarded,
}

/// The scope of every persisted content, cell or Lakehouse edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveEffect {
    /// The complete IPYNB document is replaced, not an atomic per-cell patch.
    WholeNotebook,
}

/// Supported persisted IPYNB cell types; none implies live execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CellType {
    /// Executable source with saved outputs and an execution counter.
    Code,
    /// Markdown source, potentially carrying attachments.
    Markdown,
    /// Uninterpreted source, potentially carrying attachments.
    Raw,
}

impl CellType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Markdown => "markdown",
            Self::Raw => "raw",
        }
    }

    fn parse(value: &Value) -> Result<Self, NativeError> {
        match value.as_str() {
            Some("code") => Ok(Self::Code),
            Some("markdown") => Ok(Self::Markdown),
            Some("raw") => Ok(Self::Raw),
            _ => Err(NativeError::InvalidInput),
        }
    }
}

/// A new cell, or an imported cell preserving all supplied JSON fields.
#[derive(Clone, PartialEq)]
pub struct Cell {
    value: Value,
}

impl Cell {
    /// Create a cell with source lines and type-appropriate execution defaults.
    pub fn new(cell_type: CellType, source: &str) -> Self {
        let mut value = json!({
            "cell_type": cell_type.as_str(),
            "source": source_lines(source),
            "metadata": {}
        });
        if cell_type == CellType::Code {
            value["outputs"] = json!([]);
            value["execution_count"] = Value::Null;
        }
        Self { value }
    }

    /// Validate a cell without normalizing field presence, source or unknown fields.
    ///
    /// # Errors
    /// Rejects invalid types and exceeded budgets.
    pub fn from_value(value: Value, limits: ContentLimits) -> Result<Self, NativeError> {
        limits.validate()?;
        encoded_len(&value, limits.document_bytes)?;
        validate_cell(&value, limits)?;
        Ok(Self { value })
    }

    /// Borrow the exact persisted shape, including saved (not live) outputs.
    pub fn as_value(&self) -> &Value {
        &self.value
    }
}

/// An explicit index bound to one immutable local content revision.
///
/// This is not a durable cell ID, an ETag, or a remote concurrency guard.
#[derive(Clone)]
pub struct CellSelector {
    index: usize,
    revision: Arc<Value>,
}

impl CellSelector {
    /// The source-relative, zero-based index selected explicitly by the caller.
    pub const fn index(&self) -> usize {
        self.index
    }
}

/// An insertion boundary bound to one immutable local content revision.
#[derive(Clone)]
pub struct CellPosition {
    index: usize,
    revision: Arc<Value>,
}

impl CellPosition {
    /// The source-relative boundary, from zero through the cell count inclusive.
    pub const fn index(&self) -> usize {
        self.index
    }
}

/// A full IPYNB snapshot retaining unknown fields and missing/null distinctions.
///
/// Clones share a revision. Every successful transform returns a new revision;
/// selectors from the old revision cannot address the transformed document.
#[derive(Clone)]
pub struct NotebookDocument {
    value: Arc<Value>,
    limits: ContentLimits,
}

impl NotebookDocument {
    /// Import bounded nbformat 4 JSON without altering its source representation.
    ///
    /// # Errors
    /// Rejects malformed documents, unsupported cell types and exceeded budgets.
    pub fn from_slice(bytes: &[u8], limits: ContentLimits) -> Result<Self, NativeError> {
        limits.validate()?;
        if bytes.len() > limits.document_bytes {
            return Err(NativeError::Limit);
        }
        let value = serde_json::from_slice(bytes).map_err(|_| NativeError::InvalidInput)?;
        Self::from_value(value, limits)
    }

    /// Validate caller-owned JSON while retaining every supplied field.
    ///
    /// # Errors
    /// Rejects malformed nbformat 4 documents and exceeded budgets.
    pub fn from_value(value: Value, limits: ContentLimits) -> Result<Self, NativeError> {
        limits.validate()?;
        validate_document(&value, limits)?;
        Ok(Self {
            value: Arc::new(value),
            limits,
        })
    }

    /// Borrow the full document. No mutation or schema projection is performed.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// The finite budgets attached to this snapshot and all derived revisions.
    pub const fn limits(&self) -> ContentLimits {
        self.limits
    }

    /// Number of persisted cells, not executions or kernel messages.
    pub fn cell_count(&self) -> usize {
        self.cells().len()
    }

    /// Declare the whole-item persistence effect of any edit.
    pub const fn save_effect(&self) -> SaveEffect {
        SaveEffect::WholeNotebook
    }

    /// Bind an explicit existing-cell index to this revision.
    ///
    /// # Errors
    /// Rejects out-of-range indices; there is no implicit default or ID fallback.
    pub fn selector(&self, index: usize) -> Result<CellSelector, NativeError> {
        if index >= self.cell_count() {
            return Err(NativeError::InvalidInput);
        }
        Ok(CellSelector {
            index,
            revision: Arc::clone(&self.value),
        })
    }

    /// Bind an explicit insertion boundary, including the end of an empty notebook.
    ///
    /// # Errors
    /// Rejects positions beyond the end.
    pub fn position(&self, index: usize) -> Result<CellPosition, NativeError> {
        if index > self.cell_count() {
            return Err(NativeError::InvalidInput);
        }
        Ok(CellPosition {
            index,
            revision: Arc::clone(&self.value),
        })
    }

    /// Borrow one cell with exact field presence and attachment representation.
    ///
    /// # Errors
    /// Rejects selectors bound to another snapshot or an obsolete revision.
    pub fn cell(&self, selector: &CellSelector) -> Result<&Value, NativeError> {
        self.check_selector(selector)?;
        Ok(&self.cells()[selector.index])
    }

    /// Borrow saved outputs, distinguishing absent, null and empty.
    ///
    /// These are stored notebook data, never a current kernel execution result.
    ///
    /// # Errors
    /// Rejects a stale selector.
    pub fn saved_outputs(&self, selector: &CellSelector) -> Result<Option<&Value>, NativeError> {
        let cell = self.cell(selector)?;
        Ok(cell.get("outputs"))
    }

    /// Replace source only, retaining the cell's type, metadata, saved outputs and ID.
    ///
    /// # Errors
    /// Rejects stale selectors and exceeded budgets without modifying this snapshot.
    pub fn replace_source(
        &self,
        selector: &CellSelector,
        source: &str,
    ) -> Result<Self, NativeError> {
        self.check_selector(selector)?;
        if source.len() > self.limits.source_bytes {
            return Err(NativeError::Limit);
        }
        self.edit(|value| {
            value["cells"][selector.index]["source"] = source_lines(source);
            Ok(())
        })
    }

    /// Apply an explicit type conversion, removing incompatible execution fields.
    ///
    /// Converting to code removes attachments and initializes outputs/count.
    /// A no-op conversion preserves the imported document exactly.
    ///
    /// # Errors
    /// Rejects a stale selector or an exceeded document budget.
    pub fn change_type(
        &self,
        selector: &CellSelector,
        cell_type: CellType,
    ) -> Result<Self, NativeError> {
        let current = CellType::parse(&self.cell(selector)?["cell_type"])?;
        if current == cell_type {
            return Ok(self.clone());
        }
        self.edit(|value| {
            let cell = value["cells"][selector.index]
                .as_object_mut()
                .ok_or(NativeError::InvalidInput)?;
            cell.insert("cell_type".into(), cell_type.as_str().into());
            if cell_type == CellType::Code {
                cell.remove("attachments");
                cell.insert("outputs".into(), json!([]));
                cell.insert("execution_count".into(), Value::Null);
            } else {
                cell.remove("outputs");
                cell.remove("execution_count");
            }
            Ok(())
        })
    }

    /// Set persisted cell language metadata; this does not change a live session.
    ///
    /// # Errors
    /// Rejects stale selectors, invalid language and malformed metadata.
    pub fn set_language(
        &self,
        selector: &CellSelector,
        language: &str,
    ) -> Result<Self, NativeError> {
        self.check_selector(selector)?;
        if language.is_empty() || language.chars().any(char::is_control) {
            return Err(NativeError::InvalidInput);
        }
        self.edit(|value| {
            object_field(&mut value["cells"][selector.index], "metadata")?
                .insert("language".into(), language.into());
            Ok(())
        })
    }

    /// Set or clear the persisted `parameters` tag while preserving other tags.
    ///
    /// # Errors
    /// Rejects stale selectors or malformed metadata/tags.
    pub fn set_parameter(
        &self,
        selector: &CellSelector,
        enabled: bool,
    ) -> Result<Self, NativeError> {
        self.check_selector(selector)?;
        self.edit(|value| {
            let metadata = object_field(&mut value["cells"][selector.index], "metadata")?;
            let tags = metadata.entry("tags").or_insert_with(|| json!([]));
            let tags = tags.as_array_mut().ok_or(NativeError::InvalidInput)?;
            if tags.iter().any(|tag| !tag.is_string()) {
                return Err(NativeError::InvalidInput);
            }
            if enabled {
                if !tags.iter().any(|tag| tag.as_str() == Some("parameters")) {
                    tags.push("parameters".into());
                }
            } else {
                tags.retain(|tag| tag.as_str() != Some("parameters"));
            }
            Ok(())
        })
    }

    /// Insert a cell at an explicitly revision-bound boundary.
    ///
    /// # Errors
    /// Rejects stale boundaries, invalid cells and exceeded budgets.
    pub fn insert(&self, position: &CellPosition, cell: Cell) -> Result<Self, NativeError> {
        if !Arc::ptr_eq(&self.value, &position.revision) || position.index > self.cell_count() {
            return Err(NativeError::InvalidInput);
        }
        validate_cell(&cell.value, self.limits)?;
        if self.cell_count() >= self.limits.cells {
            return Err(NativeError::Limit);
        }
        self.edit(|value| {
            value["cells"]
                .as_array_mut()
                .ok_or(NativeError::InvalidInput)?
                .insert(position.index, cell.value);
            Ok(())
        })
    }

    /// Append a cell, returning a new revision and requiring a whole-item save.
    ///
    /// # Errors
    /// Rejects invalid cells or exceeded budgets.
    pub fn append(&self, cell: Cell) -> Result<Self, NativeError> {
        self.insert(&self.position(self.cell_count())?, cell)
    }

    /// Delete one explicitly selected cell, shifting subsequent source indices.
    ///
    /// # Errors
    /// Rejects a stale or invalid selector without changing this snapshot.
    pub fn delete(&self, selector: &CellSelector) -> Result<Self, NativeError> {
        self.check_selector(selector)?;
        self.edit(|value| {
            value["cells"]
                .as_array_mut()
                .ok_or(NativeError::InvalidInput)?
                .remove(selector.index);
            Ok(())
        })
    }

    /// Read effective Lakehouse metadata without migrating or modifying the notebook.
    ///
    /// Non-null `dependencies`, including an empty object, shadows `trident`.
    ///
    /// # Errors
    /// Rejects malformed effective dependency or Lakehouse objects.
    pub fn default_lakehouse(&self) -> Result<Option<&Map<String, Value>>, NativeError> {
        let metadata = optional_object(self.value.get("metadata"))?;
        let Some(metadata) = metadata else {
            return Ok(None);
        };
        let branch = effective_dependencies(metadata)?;
        optional_object(branch.and_then(|branch| branch.get("lakehouse")))
    }

    /// Persist a default binding, preserving effective and legacy sibling bindings.
    ///
    /// This migrates the effective branch into `dependencies` and removes only
    /// `trident.lakehouse`. It never restarts a live session or kernel.
    ///
    /// # Errors
    /// Rejects invalid identifiers, malformed metadata and exceeded budgets.
    pub fn set_default_lakehouse(&self, binding: &DefaultLakehouse) -> Result<Self, NativeError> {
        crate::runtime::validate_segment(&binding.id)?;
        crate::runtime::validate_segment(&binding.workspace_id)?;
        self.edit(|value| {
            let metadata = object_field(value, "metadata")?;
            let mut dependencies = effective_dependencies(metadata)?
                .cloned()
                .unwrap_or_default();
            let mut lakehouse = optional_object(dependencies.get("lakehouse"))?
                .cloned()
                .unwrap_or_default();
            let mut known = match lakehouse.get("known_lakehouses") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) if items.iter().all(Value::is_object) => items.clone(),
                _ => return Err(NativeError::InvalidInput),
            };
            let mut found = false;
            for item in &mut known {
                if item.get("id").and_then(Value::as_str) == Some(binding.id.as_str()) {
                    found = true;
                    if !binding.name.is_empty() && item.get("display_name").is_some() {
                        item["display_name"] = binding.name.clone().into();
                    }
                }
            }
            if !found {
                known.push(json!({"id": binding.id, "display_name": binding.name}));
            }
            lakehouse.insert("default_lakehouse".into(), binding.id.clone().into());
            lakehouse.insert("default_lakehouse_name".into(), binding.name.clone().into());
            lakehouse.insert(
                "default_lakehouse_workspace_id".into(),
                binding.workspace_id.clone().into(),
            );
            lakehouse.insert("known_lakehouses".into(), known.into());
            dependencies.insert("lakehouse".into(), lakehouse.into());
            let remove_legacy = if let Some(Value::Object(legacy)) = metadata.get_mut("trident") {
                legacy.remove("lakehouse");
                legacy.is_empty()
            } else {
                false
            };
            if remove_legacy {
                metadata.remove("trident");
            }
            metadata.insert("dependencies".into(), dependencies.into());
            Ok(())
        })
    }

    fn cells(&self) -> &[Value] {
        self.value["cells"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
    }

    fn check_selector(&self, selector: &CellSelector) -> Result<(), NativeError> {
        if !Arc::ptr_eq(&self.value, &selector.revision) || selector.index >= self.cell_count() {
            return Err(NativeError::InvalidInput);
        }
        Ok(())
    }

    fn edit(
        &self,
        transform: impl FnOnce(&mut Value) -> Result<(), NativeError>,
    ) -> Result<Self, NativeError> {
        let mut value = (*self.value).clone();
        transform(&mut value)?;
        Self::from_value(value, self.limits)
    }
}

/// Explicit persisted default Lakehouse selection; no discovery or live reconfiguration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultLakehouse {
    /// Lakehouse artifact identifier.
    pub id: String,
    /// Display name, which may be empty.
    pub name: String,
    /// Workspace containing the Lakehouse, not necessarily the Notebook workspace.
    pub workspace_id: String,
}

fn effective_dependencies(
    metadata: &Map<String, Value>,
) -> Result<Option<&Map<String, Value>>, NativeError> {
    let branch = match metadata.get("dependencies") {
        None | Some(Value::Null) => metadata.get("trident"),
        value => value,
    };
    optional_object(branch)
}

fn optional_object(value: Option<&Value>) -> Result<Option<&Map<String, Value>>, NativeError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        _ => Err(NativeError::InvalidInput),
    }
}

fn object_field<'a>(
    value: &'a mut Value,
    field: &str,
) -> Result<&'a mut Map<String, Value>, NativeError> {
    let object = value.as_object_mut().ok_or(NativeError::InvalidInput)?;
    let entry = object.entry(field).or_insert_with(|| json!({}));
    if entry.is_null() {
        *entry = json!({});
    }
    entry.as_object_mut().ok_or(NativeError::InvalidInput)
}

fn source_lines(source: &str) -> Value {
    Value::Array(source.split_inclusive('\n').map(Value::from).collect())
}

pub(super) fn validate_document(value: &Value, limits: ContentLimits) -> Result<(), NativeError> {
    limits.validate()?;
    encoded_len(value, limits.document_bytes)?;
    if !value.is_object()
        || value.get("nbformat").and_then(Value::as_u64) != Some(4)
        || value
            .get("nbformat_minor")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err(NativeError::InvalidInput);
    }
    optional_object(value.get("metadata"))?;
    let cells = value
        .get("cells")
        .and_then(Value::as_array)
        .ok_or(NativeError::InvalidInput)?;
    if cells.len() > limits.cells {
        return Err(NativeError::Limit);
    }
    for cell in cells {
        validate_cell(cell, limits)?;
    }
    Ok(())
}

fn validate_cell(cell: &Value, limits: ContentLimits) -> Result<(), NativeError> {
    if !cell.is_object() {
        return Err(NativeError::InvalidInput);
    }
    CellType::parse(&cell["cell_type"])?;
    let source_len = match cell.get("source") {
        Some(Value::String(source)) => source.len(),
        Some(Value::Array(lines)) => {
            let mut bytes = 0usize;
            for line in lines {
                bytes = bytes
                    .checked_add(line.as_str().ok_or(NativeError::InvalidInput)?.len())
                    .ok_or(NativeError::Limit)?;
            }
            bytes
        }
        _ => return Err(NativeError::InvalidInput),
    };
    if source_len > limits.source_bytes {
        return Err(NativeError::Limit);
    }
    optional_object(cell.get("metadata"))?;
    optional_object(cell.get("attachments"))?;
    if cell
        .get("id")
        .is_some_and(|id| !id.is_null() && !id.is_string())
    {
        return Err(NativeError::InvalidInput);
    }
    if let Some(count) = cell.get("execution_count") {
        if !count.is_null() && count.as_u64().is_none() {
            return Err(NativeError::InvalidInput);
        }
    }
    match cell.get("outputs") {
        None | Some(Value::Null) => {}
        Some(Value::Array(outputs)) => {
            if outputs.len() > limits.outputs_per_cell {
                return Err(NativeError::Limit);
            }
            if outputs.iter().any(|output| !output.is_object()) {
                return Err(NativeError::InvalidInput);
            }
        }
        _ => return Err(NativeError::InvalidInput),
    }
    Ok(())
}

pub(super) fn encoded_len<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<usize, NativeError> {
    struct Counter {
        count: usize,
        limit: usize,
    }
    impl io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.count) {
                return Err(io::Error::other("JSON byte budget exceeded"));
            }
            self.count += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { count: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| NativeError::Limit)?;
    Ok(counter.count)
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use serde::ser::SerializeStruct;

    struct BorrowedPayload<'a> {
        text: &'a str,
        tail_visits: &'a std::cell::Cell<usize>,
    }

    impl Serialize for BorrowedPayload<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut fields = serializer.serialize_struct("BorrowedPayload", 2)?;
            fields.serialize_field("text", self.text)?;
            self.tail_visits.set(self.tail_visits.get() + 1);
            fields.serialize_field("tail", &true)?;
            fields.end()
        }
    }

    #[test]
    fn borrowed_sizing_stops_serialization_at_the_limit_without_visiting_the_tail() {
        let source = "\"\\\n".repeat(64 * 1024);
        let tail_visits = std::cell::Cell::new(0);
        let payload = BorrowedPayload {
            text: &source,
            tail_visits: &tail_visits,
        };
        assert!(matches!(encoded_len(&payload, 64), Err(NativeError::Limit)));
        assert_eq!(tail_visits.get(), 0);
        let expected = serde_json::to_vec(&payload).unwrap().len();
        assert_eq!(tail_visits.get(), 1);
        assert_eq!(encoded_len(&payload, expected), Ok(expected));
        assert_eq!(tail_visits.get(), 2);
        assert_eq!(source.len(), 3 * 64 * 1024);
    }

    #[test]
    fn borrowed_unsized_text_counts_escaped_json_bytes_exactly() {
        let text = "\"\\\n\0";
        let expected = serde_json::to_vec(text).unwrap().len();
        assert_eq!(encoded_len(text, expected), Ok(expected));
        assert!(matches!(
            encoded_len(text, expected - 1),
            Err(NativeError::Limit)
        ));
    }
}
