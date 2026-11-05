// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Closed native scripting envelopes and transactional single-submission execution.

use crate::{client::*, models::*, workload::*};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::marker::PhantomData;

/// Source-supported script kinds; table is the native TOM dataSource, not `dataSource`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScriptKind {
    /// Root expression.
    Expression,
    /// Root TOM table, matching model.tables.
    Table,
    /// Root entity type.
    Entity,
    /// Root semantic entity relationship.
    EntityRelationship,
    /// Root physical TOM relationship.
    Relationship,
    /// Root natural-language rule; no LLM execution implied.
    Rule,
    /// Root namespace.
    Namespace,
    /// Root metric; no direct resource write endpoints.
    Metric,
    /// Root reusable property; separately feature-gated, no direct resource writes.
    ReusableProperty,
    /// Child of a table.
    Column,
    /// Child of a table.
    Partition,
    /// Child table measure, distinct from a root metric.
    Measure,
    /// Child entity property.
    Property,
    /// Child entity additional backing-table reference.
    AdditionalBackingTable,
    /// Child rule entity reference.
    ReferencedEntity,
    /// Child rule relationship reference; not alterable.
    ReferencedRelationship,
}

impl ScriptKind {
    /// Exact native discriminator/key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Table => "table",
            Self::Entity => "entity",
            Self::EntityRelationship => "entityRelationship",
            Self::Relationship => "relationship",
            Self::Rule => "rule",
            Self::Namespace => "namespace",
            Self::Metric => "metric",
            Self::ReusableProperty => "reusableProperty",
            Self::Column => "column",
            Self::Partition => "partition",
            Self::Measure => "measure",
            Self::Property => "property",
            Self::AdditionalBackingTable => "additionalBackingTable",
            Self::ReferencedEntity => "referencedEntity",
            Self::ReferencedRelationship => "referencedRelationship",
        }
    }
    /// Exact owning object key, absent for root kinds.
    pub fn parent_key(self) -> Option<&'static str> {
        match self {
            Self::Column | Self::Partition | Self::Measure => Some("table"),
            Self::Property | Self::AdditionalBackingTable => Some("entity"),
            Self::ReferencedEntity | Self::ReferencedRelationship => Some("rule"),
            _ => None,
        }
    }
    fn parse(value: &str) -> Result<Self, WorkloadError> {
        serde_json::from_value(Value::String(value.to_owned()))
            .map_err(|_| WorkloadError::invalid("script kind"))
    }
    fn validate_name(self, name: &str) -> Result<(), WorkloadError> {
        validate_segment(
            name,
            match self {
                Self::Namespace => 2048,
                Self::ReusableProperty => 128,
                _ => 512,
            },
        )?;
        if matches!(
            self,
            Self::Entity | Self::EntityRelationship | Self::Rule | Self::Metric
        ) {
            validate_composite_name(name)?;
        }
        Ok(())
    }
}

mod sealed {
    pub trait Script {}
    pub trait Root {}
    pub trait Child {}
    pub trait Alter {}
}

/// Sealed source-defined payload/reference pairing.
pub trait ScriptObject: NativeModel + sealed::Script {
    /// Native kind that owns this DTO.
    const KIND: ScriptKind;
}
/// Root scriptable object.
pub trait RootScriptObject: ScriptObject + sealed::Root {}
/// Child scriptable object with exactly one named parent.
pub trait ChildScriptObject: ScriptObject + sealed::Child {}
/// Kinds whose script alter projection exists.
pub trait AlterableScriptObject: ScriptObject + sealed::Alter {}

macro_rules! script_object {
    ($model:ty, $kind:ident, $scope:ident) => {
        impl sealed::Script for $model {}
        impl ScriptObject for $model {
            const KIND: ScriptKind = ScriptKind::$kind;
        }
        impl sealed::$scope for $model {}
    };
}
script_object!(Expression, Expression, Root);
script_object!(DataSource, Table, Root);
script_object!(EntityType, Entity, Root);
script_object!(EntityRelationship, EntityRelationship, Root);
script_object!(Relationship, Relationship, Root);
script_object!(Rule, Rule, Root);
script_object!(Namespace, Namespace, Root);
script_object!(Metric, Metric, Root);
script_object!(ReusableProperty, ReusableProperty, Root);
script_object!(Column, Column, Child);
script_object!(Partition, Partition, Child);
script_object!(Measure, Measure, Child);
script_object!(EntityProperty, Property, Child);
script_object!(AdditionalBackingTable, AdditionalBackingTable, Child);
script_object!(ReferencedEntity, ReferencedEntity, Child);
script_object!(ReferencedRelationship, ReferencedRelationship, Child);
macro_rules! root { ($($ty:ty),+) => { $(impl RootScriptObject for $ty {})+ }; }
macro_rules! child { ($($ty:ty),+) => { $(impl ChildScriptObject for $ty {})+ }; }
macro_rules! alter { ($($ty:ty),+) => { $(impl sealed::Alter for $ty {} impl AlterableScriptObject for $ty {})+ }; }
root!(
    Expression,
    DataSource,
    EntityType,
    EntityRelationship,
    Relationship,
    Rule,
    Namespace,
    Metric,
    ReusableProperty
);
child!(
    Column,
    Partition,
    Measure,
    EntityProperty,
    AdditionalBackingTable,
    ReferencedEntity,
    ReferencedRelationship
);
alter!(
    Expression,
    DataSource,
    EntityType,
    EntityRelationship,
    Relationship,
    Rule,
    Namespace,
    Metric,
    ReusableProperty,
    Column,
    Partition,
    Measure,
    EntityProperty,
    AdditionalBackingTable,
    ReferencedEntity
);

/// Closed by-NAME reference. It never accepts lineageTag or additional keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectReference {
    kind: ScriptKind,
    name: String,
    parent: Option<String>,
}

impl ObjectReference {
    /// Validate one of the source's exact closed reference shapes.
    pub fn from_value(value: Value) -> Result<Self, WorkloadError> {
        let map = value
            .as_object()
            .ok_or_else(|| WorkloadError::invalid("closed script reference"))?;
        let mut candidate = None;
        for key in map.keys() {
            let kind = ScriptKind::parse(key)?;
            if map.len() == 1 && kind.parent_key().is_none()
                || map.len() == 2
                    && kind
                        .parent_key()
                        .is_some_and(|parent| map.contains_key(parent))
            {
                if candidate.is_some() {
                    return Err(WorkloadError::invalid("ambiguous script reference"));
                }
                candidate = Some(kind);
            }
        }
        let kind =
            candidate.ok_or_else(|| WorkloadError::invalid("closed script reference keys"))?;
        let name = map[kind.key()]
            .as_str()
            .ok_or_else(|| WorkloadError::invalid("script reference name type"))?
            .to_owned();
        kind.validate_name(&name)?;
        let parent = match kind.parent_key() {
            Some(key) => {
                let name = map[key]
                    .as_str()
                    .ok_or_else(|| WorkloadError::invalid("script reference parent type"))?;
                validate_segment(name, 512)?;
                if key == "entity" || key == "rule" {
                    validate_composite_name(name)?;
                }
                Some(name.to_owned())
            }
            None => None,
        };
        Ok(Self { kind, name, parent })
    }
    /// Reference kind, not inferred from a REST ID.
    pub fn kind(&self) -> ScriptKind {
        self.kind
    }
    /// Exact object name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Exact owning object's name, when this is a child.
    pub fn parent_name(&self) -> Option<&str> {
        self.parent.as_deref()
    }
    /// Exact closed native shape.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        if let (Some(key), Some(name)) = (self.kind.parent_key(), &self.parent) {
            map.insert(key.into(), Value::String(name.clone()));
        }
        map.insert(self.kind.key().into(), Value::String(self.name.clone()));
        Value::Object(map)
    }
}
impl Serialize for ObjectReference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for ObjectReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Type-branded by-name script reference; cannot accidentally use an entity ref for a column.
#[derive(Clone, Debug)]
pub struct ScriptReference<T: ScriptObject> {
    reference: ObjectReference,
    marker: PhantomData<T>,
}
impl<T: RootScriptObject> ScriptReference<T> {
    /// A root reference with exactly the selected kind key.
    pub fn root(name: ObjectName) -> Result<Self, WorkloadError> {
        T::KIND.validate_name(name.as_str())?;
        Ok(Self {
            reference: ObjectReference {
                kind: T::KIND,
                name: name.as_str().into(),
                parent: None,
            },
            marker: PhantomData,
        })
    }
}
impl<T: ChildScriptObject> ScriptReference<T> {
    /// A child reference with exactly the source-required parent and child name.
    pub fn child(parent: ObjectName, name: ObjectName) -> Result<Self, WorkloadError> {
        let reference = ObjectReference::from_value(json!({
            T::KIND.parent_key().expect("sealed child kind"): parent.as_str(),
            T::KIND.key(): name.as_str()
        }))?;
        Ok(Self {
            reference,
            marker: PhantomData,
        })
    }
}

/// Create references the parent collection, not the object itself.
#[derive(Clone, Debug)]
pub struct ScriptParent<T: ScriptObject> {
    value: Value,
    marker: PhantomData<T>,
}
impl<T: RootScriptObject> ScriptParent<T> {
    /// Exact empty root parent `{}`.
    pub fn root() -> Self {
        Self {
            value: json!({}),
            marker: PhantomData,
        }
    }
}
impl<T: ChildScriptObject> ScriptParent<T> {
    /// Exact parent key for this child type.
    pub fn named(name: ObjectName) -> Result<Self, WorkloadError> {
        validate_segment(name.as_str(), 512)?;
        if matches!(T::KIND.parent_key(), Some("entity" | "rule")) {
            validate_composite_name(name.as_str())?;
        }
        Ok(Self {
            value: json!({ T::KIND.parent_key().expect("sealed child kind"): name.as_str() }),
            marker: PhantomData,
        })
    }
}

/// Leaf verbs only; sequence nesting cannot be represented here.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScriptVerb {
    /// Strict create, failing if the target already exists.
    Create,
    /// Wholesale object upsert; body may rename the addressed object.
    CreateOrReplace,
    /// Merge patch of an existing object.
    Alter,
    /// Remove an existing object.
    Delete,
}
impl ScriptVerb {
    /// Exact native leaf verb key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::CreateOrReplace => "createOrReplace",
            Self::Alter => "alter",
            Self::Delete => "delete",
        }
    }
}

/// Validated native leaf with exact wire envelope and identity/effect metadata.
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct ScriptLeaf {
    value: Value,
    #[serde(skip)]
    verb: ScriptVerb,
    #[serde(skip)]
    target: ObjectReference,
    #[serde(skip)]
    may_remove_metric_metadata: bool,
}

impl ScriptLeaf {
    /// Build a strict create using only its correct parent shape and typed projection.
    pub fn create<T: ScriptObject>(
        parent: ScriptParent<T>,
        body: &T,
    ) -> Result<Self, WorkloadError> {
        Self::from_value(
            json!({"create": { "parentObject": parent.value, T::KIND.key(): to_value(body)? }}),
        )
    }
    /// Address the object itself, by name, and replace/upsert its complete create projection.
    pub fn create_or_replace<T: ScriptObject>(
        target: &ScriptReference<T>,
        body: &T,
    ) -> Result<Self, WorkloadError> {
        Self::from_value(
            json!({"createOrReplace": { "object": target.reference.to_value(), T::KIND.key(): to_value(body)? }}),
        )
    }
    /// Alter only a source-supported kind with its own closed patch schema.
    pub fn alter<T: AlterableScriptObject>(
        target: &ScriptReference<T>,
        patch: &MergePatch<T>,
    ) -> Result<Self, WorkloadError> {
        Self::from_value(
            json!({"alter": { "object": target.reference.to_value(), T::KIND.key(): patch.value() }}),
        )
    }
    /// Delete one typed reference by name; never by REST lineage tag.
    pub fn delete<T: ScriptObject>(target: &ScriptReference<T>) -> Result<Self, WorkloadError> {
        Self::from_value(json!({"delete": { "object": target.reference.to_value() }}))
    }
    /// Validate an external leaf script, rejecting mixed operations, refs, kinds and unknown writes.
    pub fn from_value(value: Value) -> Result<Self, WorkloadError> {
        let outer = value
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or_else(|| WorkloadError::invalid("exactly one leaf verb"))?;
        let (verb_key, body) = outer.iter().next().expect("single key");
        let verb: ScriptVerb = serde_json::from_value(Value::String(verb_key.clone()))
            .map_err(|_| WorkloadError::invalid("leaf script verb; no nested sequence"))?;
        let map = body
            .as_object()
            .ok_or_else(|| WorkloadError::invalid("leaf body object"))?;
        if verb == ScriptVerb::Delete {
            if map.len() != 1 {
                return Err(WorkloadError::invalid("delete body keys"));
            }
            let reference = ObjectReference::from_value(
                map.get("object")
                    .cloned()
                    .ok_or_else(|| WorkloadError::invalid("delete object ref"))?,
            )?;
            return Ok(Self {
                value,
                verb,
                target: reference,
                may_remove_metric_metadata: false,
            });
        }
        let identity_key = if verb == ScriptVerb::Create {
            "parentObject"
        } else {
            "object"
        };
        let objects: Vec<_> = map
            .iter()
            .filter(|(k, _)| k.as_str() != identity_key)
            .collect();
        if objects.len() != 1 {
            return Err(WorkloadError::invalid("exactly one native script kind"));
        }
        let (kind_key, payload) = objects[0];
        let kind = ScriptKind::parse(kind_key)?;
        if verb == ScriptVerb::Alter && kind == ScriptKind::ReferencedRelationship {
            return Err(WorkloadError::gap(
                "referencedRelationship has no alter projection",
            ));
        }
        check_body(kind, payload, verb == ScriptVerb::Alter)?;
        let target = if verb == ScriptVerb::Create {
            let parent = map.get("parentObject");
            let name_key = match kind {
                ScriptKind::AdditionalBackingTable => "table",
                ScriptKind::ReferencedEntity => "entity",
                ScriptKind::ReferencedRelationship => "entityRelationship",
                _ => "name",
            };
            let name = payload
                .get(name_key)
                .and_then(Value::as_str)
                .ok_or_else(|| WorkloadError::invalid("script create name"))?;
            kind.validate_name(name)?;
            match kind.parent_key() {
                Some(parent_key) => {
                    let parent = parent
                        .and_then(Value::as_object)
                        .filter(|o| o.len() == 1 && o.contains_key(parent_key))
                        .ok_or_else(|| WorkloadError::invalid("closed required create parent"))?;
                    ObjectReference::from_value(
                        json!({parent_key: parent[parent_key], kind.key(): name}),
                    )?
                }
                None => {
                    if parent.is_some_and(|p| p.as_object().is_none_or(|o| !o.is_empty())) {
                        return Err(WorkloadError::invalid(
                            "root parent must be empty or omitted",
                        ));
                    }
                    ObjectReference::from_value(json!({kind.key(): name}))?
                }
            }
        } else {
            let reference = ObjectReference::from_value(
                map.get("object")
                    .cloned()
                    .ok_or_else(|| WorkloadError::invalid("script object ref"))?,
            )?;
            if reference.kind != kind {
                return Err(WorkloadError::invalid(
                    "script reference/body kind mismatch",
                ));
            }
            reference
        };
        let may_remove_metric_metadata = kind == ScriptKind::Measure
            && payload
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name != target.name());
        Ok(Self {
            value,
            verb,
            target,
            may_remove_metric_metadata,
        })
    }
}

fn check_body(kind: ScriptKind, value: &Value, patch: bool) -> Result<(), WorkloadError> {
    if kind.parent_key().is_none() && value.get("lineageTag").is_some() {
        return Err(WorkloadError::invalid(
            "server-minted root lineage tag is read-only",
        ));
    }
    match kind {
        ScriptKind::Expression => Expression::check(value, patch),
        ScriptKind::Table => DataSource::check(value, patch),
        ScriptKind::Entity => EntityType::check(value, patch),
        ScriptKind::EntityRelationship => EntityRelationship::check(value, patch),
        ScriptKind::Relationship => Relationship::check(value, patch),
        ScriptKind::Rule => Rule::check(value, patch),
        ScriptKind::Namespace => Namespace::check(value, patch),
        ScriptKind::Metric => Metric::check(value, patch),
        ScriptKind::ReusableProperty => ReusableProperty::check(value, patch),
        ScriptKind::Column => Column::check(value, patch),
        ScriptKind::Partition => Partition::check(value, patch),
        ScriptKind::Measure => Measure::check(value, patch),
        ScriptKind::Property => EntityProperty::check(value, patch),
        ScriptKind::AdditionalBackingTable => AdditionalBackingTable::check(value, patch),
        ScriptKind::ReferencedEntity => ReferencedEntity::check(value, patch),
        ScriptKind::ReferencedRelationship => ReferencedRelationship::check(value, patch),
    }
}

/// One leaf or one flat transactional sequence. Construction is always schema-checked.
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct ScriptRequest {
    value: Value,
    #[serde(skip)]
    operations: Vec<ScriptLeaf>,
}
impl ScriptRequest {
    /// One already validated native operation.
    pub fn single(operation: ScriptLeaf) -> Self {
        Self {
            value: operation.value.clone(),
            operations: vec![operation],
        }
    }
    /// Flat ordered sequence of 1..=1000 leaves; submitted exactly once.
    pub fn sequence(operations: Vec<ScriptLeaf>) -> Result<Self, WorkloadError> {
        if operations.is_empty() || operations.len() > 1000 {
            return Err(WorkloadError::invalid("sequence operation bound"));
        }
        Ok(Self {
            value: json!({"sequence":{"operations":operations.iter().map(|o| &o.value).collect::<Vec<_>>()}}),
            operations,
        })
    }
    /// Parse an external native script into the same validated deterministic contract.
    pub fn from_value(value: Value) -> Result<Self, WorkloadError> {
        let map = value
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or_else(|| WorkloadError::invalid("one top-level script verb"))?;
        if let Some(sequence) = map.get("sequence") {
            let body = sequence
                .as_object()
                .filter(|o| o.len() == 1)
                .ok_or_else(|| WorkloadError::invalid("sequence body keys"))?;
            let operations = body
                .get("operations")
                .and_then(Value::as_array)
                .ok_or_else(|| WorkloadError::invalid("sequence operations array"))?;
            if operations.is_empty() || operations.len() > 1000 {
                return Err(WorkloadError::invalid("sequence operation bound"));
            }
            return Self::sequence(
                operations
                    .iter()
                    .cloned()
                    .map(ScriptLeaf::from_value)
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        Ok(Self::single(ScriptLeaf::from_value(value)?))
    }
    /// Exact native script, not a local replay or simulation.
    pub fn value(&self) -> &Value {
        &self.value
    }
    /// Source-side table-measure rename caveat; no hidden metric-binding update is performed.
    pub fn may_remove_authored_metric_metadata(&self) -> bool {
        self.operations.iter().any(|o| o.may_remove_metric_metadata)
    }
}
impl<'de> Deserialize<'de> for ScriptRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_value(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// One validated source operation result with unknown object fields retained.
#[derive(Clone, Debug)]
pub struct OperationResult {
    /// Ordered zero-based operation index.
    pub op_index: usize,
    /// Applied leaf verb.
    pub verb: ScriptVerb,
    /// Closed by-name returned target.
    pub target: ObjectReference,
    /// Original read object. Some(Null) and None preserve null versus omission.
    ///
    /// A target minimized out of storage or removed by a later sequence operation
    /// may have no result object despite the successful transaction.
    pub object: Option<Value>,
    /// Typed known read object, when the native result contains an object.
    pub typed_object: Option<ScriptResultObject>,
    /// True only for delete; omitted for other verbs.
    pub deleted: Option<bool>,
    /// Exact original result including unknown metadata.
    pub raw: Value,
}

/// Transaction result; the SDK never treats partial/malformed results as completed.
#[derive(Clone, Debug)]
pub struct ScriptResult {
    /// Per-leaf results in native application order.
    pub results: Vec<OperationResult>,
    /// Exact response envelope.
    pub raw: Value,
}

/// Native script result projected to its source-defined kind; raw fields remain on each resource.
#[derive(Clone, Debug)]
pub enum ScriptResultObject {
    /// Named expression.
    Expression(NativeResource<Expression>),
    /// TOM table.
    Table(NativeResource<DataSource>),
    /// Entity type.
    Entity(NativeResource<EntityType>),
    /// Semantic relationship.
    EntityRelationship(NativeResource<EntityRelationship>),
    /// TOM relationship.
    Relationship(NativeResource<Relationship>),
    /// Rule.
    Rule(NativeResource<Rule>),
    /// Namespace.
    Namespace(NativeResource<Namespace>),
    /// Root metric, distinct from a table measure.
    Metric(NativeResource<Metric>),
    /// Reusable property.
    ReusableProperty(NativeResource<ReusableProperty>),
    /// Table column.
    Column(NativeResource<Column>),
    /// Table partition.
    Partition(NativeResource<Partition>),
    /// Table measure.
    Measure(NativeResource<Measure>),
    /// Entity property.
    Property(NativeResource<EntityProperty>),
    /// Additional backing table.
    AdditionalBackingTable(NativeResource<AdditionalBackingTable>),
    /// Rule entity reference.
    ReferencedEntity(NativeResource<ReferencedEntity>),
    /// Rule relationship reference.
    ReferencedRelationship(NativeResource<ReferencedRelationship>),
}

fn decode_object(kind: ScriptKind, raw: Value) -> Result<ScriptResultObject, WorkloadError> {
    Ok(match kind {
        ScriptKind::Expression => ScriptResultObject::Expression(NativeResource::decode(raw)?),
        ScriptKind::Table => ScriptResultObject::Table(NativeResource::decode(raw)?),
        ScriptKind::Entity => ScriptResultObject::Entity(NativeResource::decode(raw)?),
        ScriptKind::EntityRelationship => {
            ScriptResultObject::EntityRelationship(NativeResource::decode(raw)?)
        }
        ScriptKind::Relationship => ScriptResultObject::Relationship(NativeResource::decode(raw)?),
        ScriptKind::Rule => ScriptResultObject::Rule(NativeResource::decode(raw)?),
        ScriptKind::Namespace => ScriptResultObject::Namespace(NativeResource::decode(raw)?),
        ScriptKind::Metric => ScriptResultObject::Metric(NativeResource::decode(raw)?),
        ScriptKind::ReusableProperty => {
            ScriptResultObject::ReusableProperty(NativeResource::decode(raw)?)
        }
        ScriptKind::Column => ScriptResultObject::Column(NativeResource::decode(raw)?),
        ScriptKind::Partition => ScriptResultObject::Partition(NativeResource::decode(raw)?),
        ScriptKind::Measure => ScriptResultObject::Measure(NativeResource::decode(raw)?),
        ScriptKind::Property => ScriptResultObject::Property(NativeResource::decode(raw)?),
        ScriptKind::AdditionalBackingTable => {
            ScriptResultObject::AdditionalBackingTable(NativeResource::decode(raw)?)
        }
        ScriptKind::ReferencedEntity => {
            ScriptResultObject::ReferencedEntity(NativeResource::decode(raw)?)
        }
        ScriptKind::ReferencedRelationship => {
            ScriptResultObject::ReferencedRelationship(NativeResource::decode(raw)?)
        }
    })
}

impl OntologyClient<'_> {
    /// Generate one native transaction with explicit deferred validation semantics.
    ///
    /// DeferValidation relaxes only server-selected intermediate metric rules.
    /// Other rules still run per leaf; final state is always fully validated.
    pub fn script_request(
        &self,
        script: &ScriptRequest,
        defer_validation: bool,
    ) -> Result<WorkloadRequest, WorkloadError> {
        for operation in &script.operations {
            if operation.target.kind == ScriptKind::ReusableProperty
                && !self.target.features().reusable_properties
            {
                return Err(WorkloadError::gap(
                    "reusable-properties scripting feature unavailable",
                ));
            }
        }
        self.gate_body(script.value())?;
        let query = if defer_validation {
            vec![("deferValidation", "true")]
        } else {
            Vec::new()
        };
        self.request(
            &["definition", "scripts"],
            &query,
            WorkloadMethod::Post,
            Some("application/json"),
            to_bytes(script, self.limits.request)?,
            self.limits.response,
        )
    }

    /// Submit the entire script exactly once; server validation/conflicts are not replayed.
    ///
    /// Source guarantees all-or-nothing transaction semantics. Transport uncertainty
    /// or malformed results remain Unknown, not inferred partial commit or rollback.
    pub async fn execute_script(
        &self,
        script: &ScriptRequest,
        defer_validation: bool,
    ) -> Result<Outcome<ScriptResult>, WorkloadError> {
        let request = self.script_request(script, defer_validation)?;
        self.json_result(&request, 200, |value| decode_result(value, script))
            .await
    }
}

fn decode_result(value: Value, script: &ScriptRequest) -> Result<ScriptResult, WorkloadError> {
    let rows = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| WorkloadError::invalid("script result array"))?;
    if rows.len() != script.operations.len() {
        return Err(WorkloadError::invalid("transaction result count"));
    }
    let mut results = Vec::new();
    for (index, (row, operation)) in rows.iter().zip(&script.operations).enumerate() {
        let op_index = row
            .get("opIndex")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(|| WorkloadError::invalid("script result index"))?;
        let verb: ScriptVerb = serde_json::from_value(
            row.get("verb")
                .cloned()
                .ok_or_else(|| WorkloadError::invalid("script result verb"))?,
        )
        .map_err(|_| WorkloadError::invalid("script result verb"))?;
        let target = ObjectReference::from_value(
            row.get("target")
                .cloned()
                .ok_or_else(|| WorkloadError::invalid("script result target"))?,
        )?;
        if op_index != index
            || verb != operation.verb
            || target.kind != operation.target.kind
            || target.parent != operation.target.parent
        {
            return Err(WorkloadError::invalid("script result operation mismatch"));
        }
        let object = row.get("object").cloned();
        let deleted = match row.get("deleted") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_bool()
                    .ok_or_else(|| WorkloadError::invalid("script deleted flag"))?,
            ),
        };
        let typed_object = if verb == ScriptVerb::Delete {
            if target != operation.target
                || deleted != Some(true)
                || object.as_ref().is_some_and(|v| !v.is_null())
            {
                return Err(WorkloadError::invalid("delete result contract"));
            }
            None
        } else {
            if deleted.is_some()
                || object
                    .as_ref()
                    .is_some_and(|v| !v.is_object() && !v.is_null())
            {
                return Err(WorkloadError::invalid("write result contract"));
            }
            // The source mapper echoes the original ref for alter/createOrReplace,
            // even when the committed object's name changed.
            if target.name != operation.target.name {
                return Err(WorkloadError::invalid("script result target name"));
            }
            object
                .as_ref()
                .filter(|v| !v.is_null())
                .map(|raw| decode_object(operation.target.kind, raw.clone()))
                .transpose()?
        };
        results.push(OperationResult {
            op_index,
            verb,
            target,
            object,
            typed_object,
            deleted,
            raw: row.clone(),
        });
    }
    Ok(ScriptResult {
        results,
        raw: value,
    })
}
