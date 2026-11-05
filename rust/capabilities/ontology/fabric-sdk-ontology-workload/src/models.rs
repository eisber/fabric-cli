// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Closed native write DTOs, schema-checked merge patches, and lossless read projections.
//!
//! These are internal V4 shapes, not the public `/v1/.../model` projection.

use crate::workload::WorkloadError;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::marker::PhantomData;

/// Internal schema implementation; sealed native DTOs are the supported consumer API.
#[doc(hidden)]
pub trait Shape {
    /// Validate a complete object or merge patch.
    fn check(value: &Value, patch: bool) -> Result<(), WorkloadError>;
    /// Project known read fields without changing the retained original response.
    fn project(value: &Value) -> Value {
        value.clone()
    }
}

macro_rules! scalar_shape {
    ($($ty:ty),+ $(,)?) => {$(
        impl Shape for $ty {
            fn check(value: &Value, _: bool) -> Result<(), WorkloadError> {
                serde_json::from_value::<Self>(value.clone())
                    .map(|_| ()).map_err(|_| WorkloadError::invalid("native field type"))
            }
        }
    )+};
}
scalar_shape!(String, bool, i32);

impl<T: Shape> Shape for Vec<T> {
    fn check(value: &Value, _: bool) -> Result<(), WorkloadError> {
        let array = value
            .as_array()
            .ok_or_else(|| WorkloadError::invalid("native array"))?;
        if array.len() > 1_000_000 {
            return Err(WorkloadError::invalid("native array bound"));
        }
        for item in array {
            T::check(item, false)?;
        }
        Ok(())
    }
    fn project(value: &Value) -> Value {
        match value.as_array() {
            Some(values) => Value::Array(values.iter().map(T::project).collect()),
            None => value.clone(),
        }
    }
}

impl<T: Shape> Shape for Box<T> {
    fn check(value: &Value, patch: bool) -> Result<(), WorkloadError> {
        T::check(value, patch)
    }
    fn project(value: &Value) -> Value {
        T::project(value)
    }
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[doc = concat!("Native ", stringify!($name), " values; no guessed fallback.")]
        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum $name {
            $(#[doc = concat!("Native `", $wire, "` value.")]
              #[serde(rename = $wire)]
              $variant,)+
        }
        scalar_shape!($name);
    };
}

macro_rules! wire_model {
    ($name:ident { required { $($req:ident : $req_ty:ty => $req_wire:literal),* $(,)? }
                  optional { $($opt:ident : $opt_ty:ty => $opt_wire:literal),* $(,)? } }) => {
        #[doc = concat!("Closed native ", stringify!($name), " authoring projection.")]
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            $(#[doc = concat!("Native `", $req_wire, "` field.")]
              #[serde(rename = $req_wire)]
              pub $req: $req_ty,)*
            $(#[doc = concat!("Native `", $opt_wire, "` field; omitted when absent.")]
              #[serde(rename = $opt_wire, default, skip_serializing_if = "Option::is_none")]
              pub $opt: Option<$opt_ty>,)*
        }
        impl Shape for $name {
            fn check(value: &Value, patch: bool) -> Result<(), WorkloadError> {
                let map = value.as_object().ok_or_else(|| WorkloadError::invalid("native object"))?;
                for (key, value) in map {
                    if patch && (key == "baseEntityType" || key == "redefines") {
                        return Err(WorkloadError::invalid("immutable inheritance field"));
                    }
                    match key.as_str() {
                        $($req_wire => if !(patch && value.is_null()) { <$req_ty>::check(value, patch)?; },)*
                        $($opt_wire => if !(patch && value.is_null()) { <$opt_ty>::check(value, patch)?; },)*
                        _ => return Err(WorkloadError::invalid("unknown or read-only native write field")),
                    }
                }
                if !patch { $(
                    if !map.contains_key($req_wire) {
                        return Err(WorkloadError::invalid("required native field"));
                    }
                )* }
                check_members(map, stringify!($name), patch)
            }
            fn project(value: &Value) -> Value {
                match value.as_object() {
                    Some(map) => {
                        let mut result = Map::new();
                        $(
                            if let Some(value) = map.get($req_wire) {
                                result.insert($req_wire.into(), <$req_ty>::project(value));
                            }
                        )*
                        $(
                            if let Some(value) = map.get($opt_wire) {
                                result.insert($opt_wire.into(), <$opt_ty>::project(value));
                            }
                        )*
                        Value::Object(result)
                    }
                    None => value.clone(),
                }
            }
        }
        impl sealed::Sealed for $name {}
        impl NativeModel for $name {}
    };
}

mod sealed {
    pub trait Sealed {}
}

/// A sealed, source-defined native authoring DTO, never an arbitrary JSON graph.
pub trait NativeModel: sealed::Sealed + Serialize + DeserializeOwned + Clone + Shape {}

/// Native JSON Merge Patch validated against a specific closed DTO schema.
///
/// Omitted keys remain omitted, explicit null is retained, and arrays are
/// replacement arrays of complete typed elements, not element-level patches.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct MergePatch<T: NativeModel> {
    value: Value,
    #[serde(skip)]
    marker: PhantomData<T>,
}

impl<T: NativeModel> MergePatch<T> {
    /// Validate every key and nested shape; reject projections and unknown fields.
    pub fn new(value: Value) -> Result<Self, WorkloadError> {
        T::check(&value, true)?;
        Ok(Self {
            value,
            marker: PhantomData,
        })
    }
    /// Exact merge patch, with null versus omission preserved.
    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl<'de, T: NativeModel> Deserialize<'de> for MergePatch<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Read-only response projection with all unknown, null and server fields retained.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeResource<T> {
    /// Typed fields recognized by this source revision.
    pub fields: T,
    /// Exact original object, including unknown nested fields and nulls; not write input.
    pub raw: Value,
}

impl<T: NativeModel> NativeResource<T> {
    pub(crate) fn decode(raw: Value) -> Result<Self, WorkloadError> {
        if !raw.is_object() {
            return Err(WorkloadError::invalid("native response object"));
        }
        let fields = serde_json::from_value(T::project(&raw))
            .map_err(|_| WorkloadError::invalid("native response fields"))?;
        Ok(Self { fields, raw })
    }
    /// Server lineage tag as returned, without coercion to a UUID or name.
    pub fn lineage_tag(&self) -> Option<&str> {
        self.raw.get("lineageTag").and_then(Value::as_str)
    }
}

wire_enum!(DataType {
    Automatic => "automatic", String => "string", Int64 => "int64",
    Double => "double", DateTime => "dateTime", Decimal => "decimal",
    Boolean => "boolean", Binary => "binary", Unknown => "unknown", Variant => "variant"
});
wire_enum!(ColumnType { Data => "data" });
wire_enum!(PartitionMode { DirectQuery => "directQuery", DirectLake => "directLake" });
wire_enum!(ExpressionKind { M => "m" });
wire_enum!(Cardinality { One => "one", Many => "many", None => "none" });
wire_enum!(CrossFiltering { OneDirection => "oneDirection", BothDirections => "bothDirections", Automatic => "automatic" });
wire_enum!(PropertyScope { None => "none", Specific => "specific", All => "all" });
wire_enum!(OverrideField { Description => "description", Synonyms => "synonyms", Annotations => "annotations" });
wire_enum!(MetricType { Explicit => "explicit", Enrichment => "enrichment", Projected => "projected" });
wire_enum!(MetricDialect { Dax => "DAX", Kql => "KQL", Sql => "SQL", Generic => "Generic", NaturalLanguage => "NaturalLanguage" });

wire_model!(Annotation { required { name: String => "name" } optional { value: String => "value" } });
wire_model!(BackingColumn { required { table: String => "table", column: String => "column" } optional {} });
wire_model!(AdditionalBackingTable {
    required { table: String => "table", relationship: String => "relationship" }
    optional { annotations: Vec<Annotation> => "annotations" }
});
wire_model!(Redefines { required { entity: String => "entity", property: String => "property" } optional {} });
wire_model!(StructField { required { name: String => "name", data_type: Box<LogicalType> => "type" } optional {} });
wire_model!(BackingField { required { name: String => "name", backing_configuration: Box<PropertyBacking> => "backingConfiguration" } optional {} });

/// Recursive native logical type. Root properties allow only Struct or TimeSeries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LogicalType {
    /// Primitive/state node.
    Primitive {
        /// Primitive native data type.
        #[serde(rename = "dataType")]
        data_type: DataType,
    },
    /// Intentionally schema-free node.
    Any,
    /// Anonymous recursive struct.
    Struct {
        /// Nonempty case-insensitively unique field collection.
        fields: Vec<StructField>,
    },
    /// A time-series value node.
    TimeSeries {
        /// Recursive element type.
        #[serde(rename = "elementType")]
        element_type: Box<LogicalType>,
    },
}

/// Recursive native backing configuration, not a row-materialization promise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum PropertyBacking {
    /// One-column binding.
    Default {
        /// Source value column.
        #[serde(rename = "valueColumn")]
        value_column: BackingColumn,
    },
    /// One backing node per logical struct field.
    Struct {
        /// Recursive backing fields.
        fields: Vec<BackingField>,
    },
    /// Ordered timeseries binding.
    TimeSeries {
        /// DateTime ordering column.
        #[serde(rename = "orderingColumn")]
        ordering_column: BackingColumn,
        /// One-column shorthand, exclusive with recursive value binding.
        #[serde(
            rename = "valueColumn",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        value_column: Option<BackingColumn>,
        /// Recursive value binding.
        #[serde(
            rename = "valueBackingConfiguration",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        value_backing_configuration: Option<Box<PropertyBacking>>,
    },
}

/// Native entity relationship backing, discriminated by type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum EntityRelationshipBacking {
    /// A TOM relationship.
    Relationship {
        /// Name of the backing relationship.
        relationship: String,
        /// Ontology annotations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Vec<Annotation>>,
    },
    /// Junction table with joins to each endpoint.
    Table {
        /// Junction table name.
        table: String,
        /// From-side TOM relationship name.
        #[serde(rename = "fromRelationship")]
        from_relationship: String,
        /// To-side TOM relationship name.
        #[serde(rename = "toRelationship")]
        to_relationship: String,
        /// Ontology annotations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Vec<Annotation>>,
    },
}

/// Native partition source; no arbitrary JSON source or query variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum PartitionSource {
    /// Inline or referenced Power Query M source.
    M {
        /// Inline M source, exclusive with expressionSource.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expression: Option<String>,
        /// Shared expression name.
        #[serde(
            rename = "expressionSource",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        expression_source: Option<String>,
    },
    /// Structured source entity.
    Entity {
        /// Source entity name.
        #[serde(rename = "entityName")]
        entity_name: String,
        /// Optional source schema.
        #[serde(
            rename = "schemaName",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        schema_name: Option<String>,
        /// Shared expression name.
        #[serde(
            rename = "expressionSource",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        expression_source: Option<String>,
    },
}

wire_model!(Column {
    required { name: String => "name" }
    optional {
        lineage_tag: String => "lineageTag", column_type: ColumnType => "type",
        data_type: DataType => "dataType", source_column: String => "sourceColumn",
        description: String => "description", is_hidden: bool => "isHidden",
        is_key: bool => "isKey", is_nullable: bool => "isNullable", is_unique: bool => "isUnique",
        format_string: String => "formatString", data_category: String => "dataCategory",
        source_lineage_tag: String => "sourceLineageTag", annotations: Vec<Annotation> => "annotations"
    }
});
wire_model!(Partition {
    required { name: String => "name", source: PartitionSource => "source" }
    optional { mode: PartitionMode => "mode", description: String => "description", annotations: Vec<Annotation> => "annotations" }
});
wire_model!(Measure {
    required { name: String => "name", expression: String => "expression" }
    optional {
        lineage_tag: String => "lineageTag", source_lineage_tag: String => "sourceLineageTag",
        description: String => "description", is_hidden: bool => "isHidden",
        format_string: String => "formatString", display_folder: String => "displayFolder",
        data_category: String => "dataCategory", annotations: Vec<Annotation> => "annotations"
    }
});
wire_model!(DataSource {
    required { name: String => "name" }
    optional {
        description: String => "description", is_hidden: bool => "isHidden",
        data_category: String => "dataCategory", source_lineage_tag: String => "sourceLineageTag",
        columns: Vec<Column> => "columns", partitions: Vec<Partition> => "partitions",
        measures: Vec<Measure> => "measures", annotations: Vec<Annotation> => "annotations"
    }
});
wire_model!(Expression {
    required { name: String => "name", expression: String => "expression" }
    optional { kind: ExpressionKind => "kind", description: String => "description", annotations: Vec<Annotation> => "annotations" }
});
wire_model!(EntityProperty {
    required { name: String => "name", data_type: String => "dataType" }
    optional {
        description: String => "description", lineage_tag: String => "lineageTag",
        complex_data_type: LogicalType => "complexDataType", redefines: Redefines => "redefines",
        overridden_metadata_fields: Vec<OverrideField> => "overriddenMetadataFields",
        backing_configuration: PropertyBacking => "backingConfiguration",
        annotations: Vec<Annotation> => "annotations", synonyms: Vec<String> => "synonyms",
        reusable_property: String => "reusableProperty"
    }
});
wire_model!(EntityType {
    required { name: String => "name" }
    optional {
        description: String => "description", backing_table: String => "backingTable",
        base_entity_type: String => "baseEntityType", overridden_metadata_fields: Vec<OverrideField> => "overriddenMetadataFields",
        key_properties: Vec<String> => "keyProperties", additional_backing_tables: Vec<AdditionalBackingTable> => "additionalBackingTables",
        properties: Vec<EntityProperty> => "properties", annotations: Vec<Annotation> => "annotations",
        synonyms: Vec<String> => "synonyms", resource_links: Vec<ResourceLink> => "resourceLinks"
    }
});
wire_model!(EntityRelationship {
    required { name: String => "name", from_entity: String => "fromEntity", to_entity: String => "toEntity" }
    optional {
        label: String => "label", description: String => "description",
        backing_configuration: EntityRelationshipBacking => "backingConfiguration",
        annotations: Vec<Annotation> => "annotations", synonyms: Vec<String> => "synonyms"
    }
});
wire_model!(Namespace {
    required { name: String => "name" }
    optional { description: String => "description", annotations: Vec<Annotation> => "annotations" }
});
wire_model!(ReferencedEntity {
    required { entity: String => "entity", property_scope: PropertyScope => "propertyScope" }
    optional { properties: Vec<String> => "properties" }
});
wire_model!(ReferencedRelationship { required { entity_relationship: String => "entityRelationship" } optional {} });
wire_model!(Rule {
    required { name: String => "name", statement: String => "statement" }
    optional {
        description: String => "description", referenced_entities: Vec<ReferencedEntity> => "referencedEntities",
        referenced_relationships: Vec<String> => "referencedRelationships",
        annotations: Vec<Annotation> => "annotations", synonyms: Vec<String> => "synonyms"
    }
});

/// TOM join shape. Single-column and multi-column fields cannot be mixed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Relationship {
    /// Single column join.
    SingleColumn {
        /// Common table and filter fields.
        #[serde(flatten)]
        common: RelationshipFields,
        /// From column.
        #[serde(
            rename = "fromColumn",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        from_column: Option<String>,
        /// To column.
        #[serde(rename = "toColumn", default, skip_serializing_if = "Option::is_none")]
        to_column: Option<String>,
    },
    /// Positional ordered multi-column join.
    MultiColumn {
        /// Common table and filter fields.
        #[serde(flatten)]
        common: RelationshipFields,
        /// Ordered from columns.
        #[serde(rename = "fromColumns")]
        from_columns: Vec<String>,
        /// Ordered to columns.
        #[serde(rename = "toColumns")]
        to_columns: Vec<String>,
    },
}

wire_model!(RelationshipFields {
    required { name: String => "name", from_table: String => "fromTable", to_table: String => "toTable" }
    optional {
        is_active: bool => "isActive", from_cardinality: Cardinality => "fromCardinality",
        to_cardinality: Cardinality => "toCardinality", cross_filtering_behavior: CrossFiltering => "crossFilteringBehavior",
        annotations: Vec<Annotation> => "annotations"
    }
});

/// A native resource link, not an instruction to resolve or access the target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ResourceLink {
    /// Fabric item reference.
    FabricItem {
        /// Direct or Variable reference.
        item: FabricItemReference,
        /// Human-readable metadata.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Ontology annotations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Vec<Annotation>>,
        /// Alternative phrases.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        synonyms: Option<Vec<String>>,
    },
}

/// Caller-authored Fabric item identity; resolvedItem is read-only and not writeable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum FabricItemReference {
    /// Exact Fabric workspace/item IDs.
    Direct {
        /// Workspace UUID.
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        /// Item UUID.
        #[serde(rename = "itemId")]
        item_id: String,
    },
    /// Variable Library ItemReference (not resolved by this SDK).
    Variable {
        /// Native variable reference syntax.
        #[serde(rename = "variableReference")]
        variable_reference: String,
    },
}

wire_model!(MetricExpression { required { dialect: MetricDialect => "dialect", expression: String => "expression" } optional {} });
wire_model!(MetricFormatString { required { expression: String => "expression" } optional {} });
wire_model!(MetricTargetScope { required { table: String => "table" } optional {} });
wire_model!(Metric {
    required { metric_type: MetricType => "type", name: String => "name" }
    optional {
        description: String => "description", expression: MetricExpression => "expression",
        format_string: String => "formatString", format_string_definition: MetricFormatString => "formatStringDefinition",
        data_type: DataType => "dataType", source_lineage_tag: String => "sourceLineageTag",
        target_scopes: Vec<MetricTargetScope> => "targetScopes", annotations: Vec<Annotation> => "annotations",
        synonyms: Vec<String> => "synonyms"
    }
});
wire_model!(ReusableProperty {
    required { name: String => "name" }
    optional {
        description: String => "description", lineage_tag: String => "lineageTag",
        annotations: Vec<Annotation> => "annotations", synonyms: Vec<String> => "synonyms"
    }
});
wire_model!(InternalModel {
    required {}
    optional {
        expressions: Vec<Expression> => "expressions", tables: Vec<DataSource> => "tables",
        relationships: Vec<Relationship> => "relationships", namespaces: Vec<Namespace> => "namespaces",
        entity_relationships: Vec<EntityRelationship> => "entityRelationships", entities: Vec<EntityType> => "entities",
        rules: Vec<Rule> => "rules", metrics: Vec<Metric> => "metrics",
        reusable_properties: Vec<ReusableProperty> => "reusableProperties", annotations: Vec<Annotation> => "annotations"
    }
});
wire_model!(InternalDefinition {
    required { model: InternalModel => "model" }
    optional { description: String => "description", compatibility_level: i32 => "compatibilityLevel" }
});

fn check_members(map: &Map<String, Value>, owner: &str, patch: bool) -> Result<(), WorkloadError> {
    if owner == "EntityRelationship" && map.get("label").is_some_and(Value::is_null) {
        return Err(WorkloadError::invalid("relationship label cannot be null"));
    }
    for (key, value) in map {
        if patch && value.is_null() {
            continue;
        }
        if let Some(text) = value.as_str() {
            let max = match key.as_str() {
                "name" if owner == "Namespace" => 2048,
                "name" if owner == "ReusableProperty" => 128,
                "name" | "table" | "column" | "entity" | "property" | "relationship"
                | "fromTable" | "toTable" | "fromEntity" | "toEntity" | "label"
                | "backingTable" | "baseEntityType" | "reusableProperty" => 512,
                "statement" => 4000,
                "description"
                    if matches!(
                        owner,
                        "EntityRelationship"
                            | "Namespace"
                            | "Rule"
                            | "ReusableProperty"
                            | "FabricLinkNode"
                    ) =>
                {
                    4000
                }
                _ => 500_000_000,
            };
            if text.encode_utf16().count() > max
                || (matches!(
                    key.as_str(),
                    "name"
                        | "table"
                        | "column"
                        | "entity"
                        | "property"
                        | "relationship"
                        | "fromTable"
                        | "toTable"
                        | "fromEntity"
                        | "toEntity"
                        | "label"
                        | "baseEntityType"
                ) && text.trim().is_empty())
                || (key == "label" && text.contains(['\r', '\n']))
            {
                return Err(WorkloadError::invalid("native string constraint"));
            }
            if key == "name"
                && matches!(
                    owner,
                    "EntityType" | "EntityRelationship" | "Rule" | "Metric"
                )
            {
                validate_composite_name(text)?;
            }
        }
        if let Some(array) = value.as_array() {
            let max = match key.as_str() {
                "properties" if owner == "EntityType" => 15997,
                "columns" => 15997,
                "properties" if owner == "ReferencedEntity" => 10,
                "referencedEntities" | "referencedRelationships" => 50,
                "partitions" | "measures" => 1_000_000,
                "fields" => 1024,
                "annotations"
                    if matches!(
                        owner,
                        "Column"
                            | "Measure"
                            | "Partition"
                            | "DataSource"
                            | "Expression"
                            | "RelationshipFields"
                            | "SingleJoinNode"
                            | "MultiJoinNode"
                            | "Metric"
                    ) =>
                {
                    1_000_000
                }
                _ => 100,
            };
            if array.len() > max {
                return Err(WorkloadError::invalid("native collection constraint"));
            }
            if key == "synonyms"
                && array.iter().any(|v| {
                    v.as_str().is_none_or(|s| {
                        s.is_empty() || s.chars().count() > 100 || s.contains(['\r', '\n'])
                    })
                })
            {
                return Err(WorkloadError::invalid("synonym constraint"));
            }
            if key == "annotations"
                && max == 100
                && array.iter().any(|a| {
                    a.get("value")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.encode_utf16().count() > 4000)
                })
            {
                return Err(WorkloadError::invalid("ontology annotation value bound"));
            }
            if matches!(
                key.as_str(),
                "keyProperties" | "referencedRelationships" | "fromColumns" | "toColumns"
            ) || (key == "properties" && owner == "ReferencedEntity")
            {
                for value in array {
                    validate_segment_string(value, 512)?;
                }
            }
            if key == "fields" {
                if array.is_empty() {
                    return Err(WorkloadError::invalid("nonempty struct fields"));
                }
                let mut names = std::collections::BTreeSet::new();
                for field in array {
                    let name = field
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| WorkloadError::invalid("struct field name"))?;
                    if !ascii_identifier(name) || !names.insert(name.to_ascii_lowercase()) {
                        return Err(WorkloadError::invalid("unique ASCII struct field names"));
                    }
                }
            }
        }
    }
    if owner == "EntityProperty" && !patch {
        let complex = map.get("dataType").and_then(Value::as_str) == Some("complex");
        if complex != map.contains_key("complexDataType") {
            return Err(WorkloadError::invalid("complex property type pairing"));
        }
        if complex
            && !matches!(
                map["complexDataType"]["kind"].as_str(),
                Some("struct" | "timeSeries")
            )
        {
            return Err(WorkloadError::invalid("complex root kind"));
        }
        if complex {
            let logical: LogicalType = serde_json::from_value(map["complexDataType"].clone())
                .map_err(|_| WorkloadError::invalid("complex property type"))?;
            let backing: Option<PropertyBacking> = map
                .get("backingConfiguration")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()
                .map_err(|_| WorkloadError::invalid("complex property backing"))?;
            check_backing(&logical, backing.as_ref())?;
        }
    }
    if owner == "ReferencedEntity" && !patch {
        let specific = map["propertyScope"] == "specific";
        if specific != map.contains_key("properties")
            || (specific && map["properties"].as_array().is_none_or(Vec::is_empty))
        {
            return Err(WorkloadError::invalid("rule property scope"));
        }
    }
    if !patch
        && owner == "TimeSeriesBackingNode"
        && (map.contains_key("valueColumn") == map.contains_key("valueBackingConfiguration"))
    {
        return Err(WorkloadError::invalid(
            "exclusive time-series value binding",
        ));
    }
    if !patch
        && owner == "MSourceNode"
        && map.contains_key("expression") == map.contains_key("expressionSource")
    {
        return Err(WorkloadError::invalid(
            "exclusive inline/shared M expression",
        ));
    }
    if !patch
        && owner == "SingleJoinNode"
        && map.contains_key("fromColumn") != map.contains_key("toColumn")
    {
        return Err(WorkloadError::invalid("paired single join columns"));
    }
    if !patch && owner == "MultiJoinNode" {
        let from = map["fromColumns"].as_array().expect("checked array");
        let to = map["toColumns"].as_array().expect("checked array");
        if from.is_empty() || from.len() != to.len() {
            return Err(WorkloadError::invalid("paired nonempty multi join columns"));
        }
    }
    if owner == "DirectItemNode" {
        for key in ["workspaceId", "itemId"] {
            if let Some(value) = map.get(key).filter(|v| !v.is_null()) {
                crate::client::validate_uuid(
                    value
                        .as_str()
                        .ok_or_else(|| WorkloadError::invalid("item reference UUID type"))?,
                )?;
            }
        }
    }
    if let Some(variable) = map.get("variableReference").and_then(Value::as_str) {
        let inner = variable
            .strip_prefix("$(/**/")
            .and_then(|s| s.strip_suffix(')'))
            .ok_or_else(|| WorkloadError::invalid("ItemReference variable syntax"))?;
        let inner = inner.strip_suffix(":ItemReference").unwrap_or(inner);
        let parts: Vec<_> = inner.split('/').collect();
        if parts.len() != 2
            || parts
                .iter()
                .any(|part| part.trim().is_empty() || part.contains(['*', ':', '(', ')']))
        {
            return Err(WorkloadError::invalid("ItemReference variable path"));
        }
    }
    Ok(())
}

fn validate_segment_string(value: &Value, max: usize) -> Result<(), WorkloadError> {
    let text = value
        .as_str()
        .ok_or_else(|| WorkloadError::invalid("name reference type"))?;
    if text.trim().is_empty() || text.encode_utf16().count() > max {
        return Err(WorkloadError::invalid("name reference bound"));
    }
    Ok(())
}

fn ascii_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn contains_struct(logical: &LogicalType) -> bool {
    match logical {
        LogicalType::Struct { .. } => true,
        LogicalType::TimeSeries { element_type } => contains_struct(element_type),
        _ => false,
    }
}

fn check_backing(
    logical: &LogicalType,
    backing: Option<&PropertyBacking>,
) -> Result<(), WorkloadError> {
    match (logical, backing) {
        (
            LogicalType::Struct { fields },
            Some(PropertyBacking::Struct {
                fields: backing_fields,
            }),
        ) => {
            if fields.len() != backing_fields.len() {
                return Err(WorkloadError::invalid("exact struct backing coverage"));
            }
            for field in fields {
                let bound = backing_fields
                    .iter()
                    .find(|bound| bound.name.eq_ignore_ascii_case(&field.name))
                    .ok_or_else(|| WorkloadError::invalid("missing struct field backing"))?;
                check_backing(&field.data_type, Some(&bound.backing_configuration))?;
            }
            Ok(())
        }
        (
            LogicalType::TimeSeries { element_type },
            Some(PropertyBacking::TimeSeries {
                value_column,
                value_backing_configuration,
                ..
            }),
        ) => {
            if value_column.is_some() == value_backing_configuration.is_some() {
                return Err(WorkloadError::invalid(
                    "exclusive time-series value binding",
                ));
            }
            if let Some(backing) = value_backing_configuration {
                check_backing(element_type, Some(backing))
            } else if contains_struct(element_type) {
                Err(WorkloadError::invalid(
                    "recursive time-series struct backing required",
                ))
            } else {
                Ok(())
            }
        }
        (_, None | Some(PropertyBacking::Default { .. })) if !contains_struct(logical) => Ok(()),
        _ => Err(WorkloadError::invalid("logical/backing shape mismatch")),
    }
}

/// Validate the native composite spelling without interpreting it as a route or lineage tag.
pub fn validate_composite_name(name: &str) -> Result<(), WorkloadError> {
    if name.trim().is_empty() || name.encode_utf16().count() > 512 {
        return Err(WorkloadError::invalid("composite name length"));
    }
    let parts: Vec<_> = name.split('#').collect();
    if parts.len() > 2 || parts.iter().any(|part| part.is_empty()) {
        return Err(WorkloadError::invalid("composite name halves"));
    }
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return Err(WorkloadError::invalid("composite name percent encoding"));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(())
}

wire_model!(PrimitiveNode { required { data_type: DataType => "dataType" } optional {} });
wire_model!(EmptyNode { required {} optional {} });
wire_model!(StructNode { required { fields: Vec<StructField> => "fields" } optional {} });
wire_model!(TimeSeriesNode { required { element_type: Box<LogicalType> => "elementType" } optional {} });
wire_model!(DefaultBackingNode { required { value_column: BackingColumn => "valueColumn" } optional {} });
wire_model!(StructBackingNode { required { fields: Vec<BackingField> => "fields" } optional {} });
wire_model!(TimeSeriesBackingNode {
    required { ordering_column: BackingColumn => "orderingColumn" }
    optional { value_column: BackingColumn => "valueColumn", value_backing_configuration: Box<PropertyBacking> => "valueBackingConfiguration" }
});
wire_model!(RelationshipBackingNode {
    required { relationship: String => "relationship" }
    optional { annotations: Vec<Annotation> => "annotations" }
});
wire_model!(TableBackingNode {
    required { table: String => "table", from_relationship: String => "fromRelationship", to_relationship: String => "toRelationship" }
    optional { annotations: Vec<Annotation> => "annotations" }
});
wire_model!(MSourceNode {
    required {}
    optional { expression: String => "expression", expression_source: String => "expressionSource" }
});
wire_model!(EntitySourceNode {
    required { entity_name: String => "entityName" }
    optional { schema_name: String => "schemaName", expression_source: String => "expressionSource" }
});
wire_model!(SingleJoinNode {
    required { name: String => "name", from_table: String => "fromTable", to_table: String => "toTable" }
    optional {
        is_active: bool => "isActive", from_cardinality: Cardinality => "fromCardinality",
        to_cardinality: Cardinality => "toCardinality", cross_filtering_behavior: CrossFiltering => "crossFilteringBehavior",
        annotations: Vec<Annotation> => "annotations", from_column: String => "fromColumn", to_column: String => "toColumn"
    }
});
wire_model!(MultiJoinNode {
    required {
        name: String => "name", from_table: String => "fromTable", to_table: String => "toTable",
        from_columns: Vec<String> => "fromColumns", to_columns: Vec<String> => "toColumns"
    }
    optional {
        is_active: bool => "isActive", from_cardinality: Cardinality => "fromCardinality",
        to_cardinality: Cardinality => "toCardinality", cross_filtering_behavior: CrossFiltering => "crossFilteringBehavior",
        annotations: Vec<Annotation> => "annotations"
    }
});
wire_model!(FabricLinkNode {
    required { item: FabricItemReference => "item" }
    optional { description: String => "description", annotations: Vec<Annotation> => "annotations", synonyms: Vec<String> => "synonyms" }
});
wire_model!(DirectItemNode { required { workspace_id: String => "workspaceId", item_id: String => "itemId" } optional {} });
wire_model!(VariableItemNode { required { variable_reference: String => "variableReference" } optional {} });

macro_rules! discriminated_shape {
    ($name:ident, $tag:literal, { $($wire:literal => $node:ty),+ $(,)? }) => {
        impl Shape for $name {
            fn check(value: &Value, patch: bool) -> Result<(), WorkloadError> {
                let mut map = value.as_object().cloned()
                    .ok_or_else(|| WorkloadError::invalid("discriminated native object"))?;
                let tag = map.remove($tag);
                let body = Value::Object(map);
                match tag.as_ref().and_then(Value::as_str) {
                    $(Some($wire) => <$node>::check(&body, patch),)+
                    None if patch && tag.is_none() => {
                        if [$(<$node>::check(&body, true).is_ok()),+].contains(&true) { Ok(()) }
                        else { Err(WorkloadError::invalid("native patch discriminator/fields")) }
                    }
                    _ => Err(WorkloadError::invalid("native discriminator")),
                }
            }
            fn project(value: &Value) -> Value {
                let Some(map) = value.as_object() else { return value.clone(); };
                let mut result = match map.get($tag).and_then(Value::as_str) {
                    $(Some($wire) => <$node>::project(value),)+
                    _ => return value.clone(),
                };
                if let Some(result_map) = result.as_object_mut() {
                    if let Some(tag) = map.get($tag) { result_map.insert($tag.into(), tag.clone()); }
                }
                result
            }
        }
        impl sealed::Sealed for $name {}
        impl NativeModel for $name {}
    };
}
discriminated_shape!(LogicalType, "kind", {
    "primitive" => PrimitiveNode, "any" => EmptyNode, "struct" => StructNode, "timeSeries" => TimeSeriesNode
});
discriminated_shape!(PropertyBacking, "type", {
    "default" => DefaultBackingNode, "struct" => StructBackingNode, "timeSeries" => TimeSeriesBackingNode
});
discriminated_shape!(EntityRelationshipBacking, "type", {
    "relationship" => RelationshipBackingNode, "table" => TableBackingNode
});
discriminated_shape!(PartitionSource, "type", { "m" => MSourceNode, "entity" => EntitySourceNode });
discriminated_shape!(Relationship, "type", { "singleColumn" => SingleJoinNode, "multiColumn" => MultiJoinNode });
discriminated_shape!(ResourceLink, "type", { "FabricItem" => FabricLinkNode });
discriminated_shape!(FabricItemReference, "type", { "Direct" => DirectItemNode, "Variable" => VariableItemNode });
