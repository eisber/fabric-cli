// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed native V4 requests and bounded result interpretation.

use crate::{models::*, workload::*};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, marker::PhantomData};

/// Caller-resolved feature applicability, not inferred from backend source existence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Features {
    /// Ontology TMSL feature explicitly available to this target.
    pub tmsl: bool,
    /// Inheritance authoring explicitly available.
    pub inheritance: bool,
    /// Reusable-properties read/script feature explicitly available.
    pub reusable_properties: bool,
    /// Expanded internal-definition projection explicitly available.
    pub auto_projection: bool,
}

/// Validated, explicit target of the private Digital Operations V4 contract.
#[derive(Clone, Debug)]
pub struct OntologyTarget {
    workload: WorkloadIdentity,
    workspace: String,
    item: String,
    features: Features,
}

impl OntologyTarget {
    /// Require exact item type, feature level 2, valid UUIDs, and TMSL applicability.
    ///
    /// `workload` is the caller's logical Digital Operations binding, not a URL
    /// override. The executor must bind it to the caller-approved capacity,
    /// tenant, audience and identity; the SDK cannot establish these permissions.
    pub fn new(
        workload: WorkloadIdentity,
        workspace: impl Into<String>,
        item: impl Into<String>,
        item_type: &str,
        feature_level: u32,
        features: Features,
    ) -> Result<Self, WorkloadError> {
        let workspace = workspace.into();
        let item = item.into();
        if workload != WorkloadIdentity::digital_operations() {
            return Err(WorkloadError::invalid(
                "Ontology requires the DigitalOperations logical workload",
            ));
        }
        if item_type != "Ontology" || feature_level != 2 {
            return Err(WorkloadError::gap("requires Ontology feature level 2"));
        }
        validate_uuid(&workspace)?;
        validate_uuid(&item)?;
        if !features.tmsl {
            return Err(WorkloadError::gap("Ontology TMSL feature unavailable"));
        }
        Ok(Self {
            workload,
            workspace,
            item,
            features,
        })
    }

    /// Explicit caller-resolved feature set.
    pub fn features(&self) -> Features {
        self.features
    }
}

pub(crate) fn validate_uuid(value: &str) -> Result<(), WorkloadError> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return Err(WorkloadError::invalid("workspace/item UUID"));
    }
    Ok(())
}

/// Native lineage identity. May be engine-derived or the reserved namespace `default`.
///
/// Never coerce it to a UUID, numeric ID or display name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineageTag(String);
impl LineageTag {
    /// Validate a nonempty lineage tag independently of names.
    pub fn new(value: impl Into<String>) -> Result<Self, WorkloadError> {
        let value = value.into();
        validate_segment(&value, 500_000_000)?;
        Ok(Self(value))
    }
    /// Exact tag, without canonicalization.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit by-name identifier, not interchangeable with lineage tags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectName(String);
impl ObjectName {
    /// Validate a name with the broad standalone namespace ceiling.
    pub fn new(value: impl Into<String>) -> Result<Self, WorkloadError> {
        let value = value.into();
        validate_segment(&value, 2048)?;
        Ok(Self(value))
    }
    /// Exact native name, encoded only when placed in a route.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn validate_segment(value: &str, max_chars: usize) -> Result<(), WorkloadError> {
    if value.trim().is_empty()
        || value.encode_utf16().count() > max_chars
        || value == "."
        || value == ".."
        || value.chars().any(char::is_control)
    {
        return Err(WorkloadError::invalid("native identity"));
    }
    Ok(())
}

mod sealed {
    pub trait Resource {}
    pub trait Mutable {}
    pub trait Lineage {}
    pub trait Named {}
}

/// Source-supported collection with typed list/get operations.
pub trait Resource: NativeModel + sealed::Resource {
    /// Exact V4 collection route segment.
    const COLLECTION: &'static str;
    /// Whether this endpoint supports explicit binding-resolution refresh.
    const REFRESH_BINDING: bool = false;
    /// Whether the source requires the reusable-properties gate.
    const REUSABLE_GATE: bool = false;
}
/// Collections with native create/merge-patch/delete endpoints.
pub trait MutableResource: Resource + sealed::Mutable {}
/// Collections whose member REST identity is a lineage tag.
pub trait LineageResource: Resource + sealed::Lineage {}
/// Collections whose member REST identity is a name.
pub trait NamedResource: Resource + sealed::Named {}

macro_rules! resource {
    ($model:ty, $route:literal, $identity:ident, $refresh:literal, $reusable:literal) => {
        impl sealed::Resource for $model {}
        impl Resource for $model {
            const COLLECTION: &'static str = $route;
            const REFRESH_BINDING: bool = $refresh;
            const REUSABLE_GATE: bool = $reusable;
        }
        impl sealed::$identity for $model {}
    };
}
resource!(EntityType, "entityTypes", Lineage, false, false);
resource!(
    EntityRelationship,
    "entityRelationships",
    Lineage,
    false,
    false
);
resource!(DataSource, "dataSources", Lineage, true, false);
resource!(Expression, "expressions", Lineage, false, false);
resource!(Namespace, "namespaces", Lineage, false, false);
resource!(Relationship, "relationships", Named, false, false);
resource!(Rule, "rules", Lineage, false, false);
resource!(Metric, "metrics", Lineage, false, false);
resource!(ReusableProperty, "reusableProperties", Named, false, true);

macro_rules! lineage { ($($model:ty),+) => { $(impl LineageResource for $model {})+ }; }
lineage!(
    EntityType,
    EntityRelationship,
    DataSource,
    Expression,
    Namespace,
    Rule,
    Metric
);
impl NamedResource for Relationship {}
impl NamedResource for ReusableProperty {}
macro_rules! mutable { ($($model:ty),+) => { $(
    impl sealed::Mutable for $model {}
    impl MutableResource for $model {}
)+ }; }
mutable!(
    EntityType,
    EntityRelationship,
    DataSource,
    Expression,
    Namespace,
    Relationship,
    Rule
);

/// Resource-branded member identity; REST name and lineage constructors are disjoint.
#[derive(Clone, Debug)]
pub struct MemberId<T: Resource> {
    value: String,
    marker: PhantomData<T>,
}
impl<T: LineageResource> MemberId<T> {
    /// Bind an explicit lineage tag to the selected REST resource type.
    pub fn lineage(tag: LineageTag) -> Self {
        Self {
            value: tag.0,
            marker: PhantomData,
        }
    }
}
impl<T: NamedResource> MemberId<T> {
    /// Bind an explicit name to a name-addressed REST resource type.
    pub fn named(name: ObjectName) -> Result<Self, WorkloadError> {
        validate_segment(name.as_str(), if T::REUSABLE_GATE { 128 } else { 512 })?;
        Ok(Self {
            value: name.0,
            marker: PhantomData,
        })
    }
}

/// Per-call local byte bounds. Limits are enforced before dispatch and after response.
#[derive(Clone, Copy, Debug)]
pub struct ByteLimits {
    /// Maximum serialized request bytes.
    pub request: usize,
    /// Maximum decoded response bytes; executor must enforce while streaming.
    pub response: usize,
}
impl ByteLimits {
    /// Reject zero limits. Native requests remain caller-bounded, not unbounded graphs.
    pub fn new(request: usize, response: usize) -> Result<Self, WorkloadError> {
        if request == 0 || response == 0 {
            return Err(WorkloadError::invalid("zero byte limit"));
        }
        Ok(Self { request, response })
    }
}

/// Exact endpoint outcome; 202 is never returned as completed.
#[derive(Clone, Debug)]
pub enum Outcome<T> {
    /// Exact expected synchronous response.
    Completed {
        /// Typed endpoint value.
        value: T,
        /// Original status, headers and body.
        response: WorkloadResponse,
    },
    /// Accepted service work; no polling or fabricated handle.
    Accepted {
        /// Original acceptance response, including Location/operation metadata if supplied.
        response: WorkloadResponse,
    },
}

/// Typed single native page, retaining its original envelope.
#[derive(Clone, Debug)]
pub struct NativePage<T> {
    /// Typed resources in source order.
    pub value: Vec<NativeResource<T>>,
    /// Opaque token; only encoded back onto the same collection route.
    pub continuation_token: Option<String>,
    /// Original envelope, including unknown metadata and continuationUri.
    pub raw: Value,
}

/// Bounded multi-page collection request.
#[derive(Clone, Debug)]
pub struct ListOptions {
    /// Maximum page dispatches.
    pub max_pages: usize,
    /// Maximum retained items; a page exceeding this bound is not silently truncated.
    pub max_items: usize,
    /// Maximum aggregate response bytes, including error/accepted responses.
    pub max_bytes: usize,
    /// Explicit binding-resolution cache refresh (may mutate cache state).
    pub refresh_binding_resolution: bool,
    /// Rule filters, repeated exact names with OR semantics, not CSV.
    pub rule_entities: Vec<ObjectName>,
}

impl ListOptions {
    /// Validate positive bounds without choosing implicit paging budgets.
    pub fn new(
        max_pages: usize,
        max_items: usize,
        max_bytes: usize,
    ) -> Result<Self, WorkloadError> {
        if max_pages == 0 || max_items == 0 || max_bytes == 0 {
            return Err(WorkloadError::invalid("zero list bound"));
        }
        Ok(Self {
            max_pages,
            max_items,
            max_bytes,
            refresh_binding_resolution: false,
            rule_entities: Vec::new(),
        })
    }
}

/// Why bounded collection traversal stopped; anything but Complete is partial.
#[derive(Clone, Debug)]
pub enum ListStop {
    /// Provider proved the end of the collection.
    Complete,
    /// A page/item/byte budget was reached; original evidence is retained.
    Limit,
    /// Provider accepted work, not a completed list.
    Accepted,
    /// Malformed/repeated token, transport failure or HTTP rejection.
    Failed(WorkloadError),
}

/// Explicit complete or partial collection, never a success-shaped empty fallback.
#[derive(Clone, Debug)]
pub struct ListResult<T> {
    /// Successfully decoded pages' items in order.
    pub items: Vec<NativeResource<T>>,
    /// Every observed bounded response, including a failed/unconsumed final page.
    pub responses: Vec<WorkloadResponse>,
    /// Token for the next unconsumed page, when known.
    pub continuation_token: Option<String>,
    /// Completion/partial classification.
    pub stop: ListStop,
}

/// Deterministic service client; caller owns private execution/authentication.
pub struct OntologyClient<'a> {
    pub(crate) target: OntologyTarget,
    pub(crate) executor: &'a dyn WorkloadExecutor,
    pub(crate) limits: ByteLimits,
}

impl<'a> OntologyClient<'a> {
    /// Compose an already validated target with one caller-owned executor.
    pub fn new(
        target: OntologyTarget,
        executor: &'a dyn WorkloadExecutor,
        limits: ByteLimits,
    ) -> Result<Self, WorkloadError> {
        ByteLimits::new(limits.request, limits.response)?;
        Ok(Self {
            target,
            executor,
            limits,
        })
    }

    pub(crate) fn route(
        &self,
        suffix: &[&str],
        query: &[(&str, &str)],
    ) -> Result<WorkloadRoute, WorkloadError> {
        let mut segments = vec![
            "v4",
            "workspaces",
            self.target.workspace.as_str(),
            "ontologies",
            self.target.item.as_str(),
        ];
        segments.extend_from_slice(suffix);
        WorkloadRoute::new(&segments, query)
    }

    pub(crate) fn request(
        &self,
        suffix: &[&str],
        query: &[(&str, &str)],
        method: WorkloadMethod,
        content_type: Option<&str>,
        body: Vec<u8>,
        response_bound: usize,
    ) -> Result<WorkloadRequest, WorkloadError> {
        WorkloadRequest::new(
            self.target.workload.clone(),
            self.route(suffix, query)?,
            method,
            content_type,
            body,
            self.limits.request,
            response_bound,
        )
    }

    pub(crate) fn gate<T: Resource>(&self) -> Result<(), WorkloadError> {
        if T::REUSABLE_GATE && !self.target.features.reusable_properties {
            return Err(WorkloadError::gap(
                "reusable-properties feature unavailable",
            ));
        }
        Ok(())
    }

    pub(crate) fn gate_body(&self, value: &Value) -> Result<(), WorkloadError> {
        match value {
            Value::Object(map) => {
                if !self.target.features.inheritance
                    && map.keys().any(|k| {
                        k == "baseEntityType" || k == "redefines" || k == "overriddenMetadataFields"
                    })
                {
                    return Err(WorkloadError::gap("entity inheritance feature unavailable"));
                }
                for value in map.values() {
                    self.gate_body(value)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.gate_body(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Generate a real strict-create request without invoking the executor.
    pub fn create_request<T: MutableResource>(
        &self,
        body: &T,
    ) -> Result<WorkloadRequest, WorkloadError> {
        self.gate::<T>()?;
        let bytes = to_bytes(body, self.limits.request)?;
        let value = serde_json::from_slice(&bytes)
            .map_err(|_| WorkloadError::invalid("serialized native object"))?;
        T::check(&value, false)?;
        self.gate_body(&value)?;
        self.request(
            &[T::COLLECTION],
            &[],
            WorkloadMethod::Post,
            Some("application/json"),
            bytes,
            self.limits.response,
        )
    }

    /// Strict native create. A 409 remains a provider error, never an upsert/retry.
    pub async fn create<T: MutableResource>(
        &self,
        body: &T,
    ) -> Result<Outcome<NativeResource<T>>, WorkloadError> {
        let request = self.create_request(body)?;
        self.resource_result(&request, 201).await
    }

    /// Generate a lineage/name-addressed read, with explicit supported cache refresh.
    pub fn get_request<T: Resource>(
        &self,
        id: &MemberId<T>,
        refresh_binding_resolution: bool,
    ) -> Result<WorkloadRequest, WorkloadError> {
        self.gate::<T>()?;
        if refresh_binding_resolution && !T::REFRESH_BINDING {
            return Err(WorkloadError::invalid(
                "binding refresh unsupported by collection",
            ));
        }
        let query = if refresh_binding_resolution {
            vec![("refreshBindingResolution", "true")]
        } else {
            Vec::new()
        };
        self.request(
            &[T::COLLECTION, &id.value],
            &query,
            WorkloadMethod::Get,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Read one native resource without falling back between name and lineage identity.
    pub async fn get<T: Resource>(
        &self,
        id: &MemberId<T>,
        refresh_binding_resolution: bool,
    ) -> Result<Outcome<NativeResource<T>>, WorkloadError> {
        let request = self.get_request(id, refresh_binding_resolution)?;
        self.resource_result(&request, 200).await
    }

    /// Generate an exact merge-patch request with omitted/null/array fidelity.
    pub fn patch_request<T: MutableResource>(
        &self,
        id: &MemberId<T>,
        patch: &MergePatch<T>,
    ) -> Result<WorkloadRequest, WorkloadError> {
        self.gate::<T>()?;
        self.gate_body(patch.value())?;
        self.request(
            &[T::COLLECTION, &id.value],
            &[],
            WorkloadMethod::Patch,
            Some("application/merge-patch+json"),
            to_bytes(patch, self.limits.request)?,
            self.limits.response,
        )
    }

    /// Apply one native merge patch; arrays replace and no client RMW/retry is performed.
    pub async fn patch<T: MutableResource>(
        &self,
        id: &MemberId<T>,
        patch: &MergePatch<T>,
    ) -> Result<Outcome<NativeResource<T>>, WorkloadError> {
        let request = self.patch_request(id, patch)?;
        self.resource_result(&request, 200).await
    }

    /// Generate a single native subresource deletion, not an Ontology item deletion.
    pub fn delete_request<T: MutableResource>(
        &self,
        id: &MemberId<T>,
    ) -> Result<WorkloadRequest, WorkloadError> {
        self.gate::<T>()?;
        self.request(
            &[T::COLLECTION, &id.value],
            &[],
            WorkloadMethod::Delete,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Delete one subresource, preserving reference/conflict errors as returned.
    pub async fn delete<T: MutableResource>(
        &self,
        id: &MemberId<T>,
    ) -> Result<Outcome<()>, WorkloadError> {
        let request = self.delete_request(id)?;
        self.empty_result(&request).await
    }

    /// Generate one collection page. Never follow continuationUri or a provider URL.
    pub fn page_request<T: Resource>(
        &self,
        token: Option<&str>,
        refresh_binding_resolution: bool,
        rule_entities: &[ObjectName],
    ) -> Result<WorkloadRequest, WorkloadError> {
        self.gate::<T>()?;
        if refresh_binding_resolution && !T::REFRESH_BINDING
            || (!rule_entities.is_empty() && T::COLLECTION != "rules")
            || rule_entities.len() > 100
        {
            return Err(WorkloadError::invalid("unsupported list parameters"));
        }
        let mut query = Vec::new();
        if let Some(token) = token {
            validate_token(token)?;
            query.push(("continuationToken", token));
        }
        if refresh_binding_resolution {
            query.push(("refreshBindingResolution", "true"));
        }
        for entity in rule_entities {
            validate_composite_name(entity.as_str())?;
            query.push(("entity", entity.as_str()));
        }
        self.request(
            &[T::COLLECTION],
            &query,
            WorkloadMethod::Get,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Read one typed page; an empty `value: []` is valid.
    pub async fn page<T: Resource>(
        &self,
        token: Option<&str>,
        refresh_binding_resolution: bool,
        rule_entities: &[ObjectName],
    ) -> Result<Outcome<NativePage<T>>, WorkloadError> {
        let request = self.page_request::<T>(token, refresh_binding_resolution, rule_entities)?;
        self.page_result(&request).await
    }

    /// Bounded traversal of one typed collection, retaining partial results and stop evidence.
    pub async fn list<T: Resource>(
        &self,
        options: &ListOptions,
    ) -> Result<ListResult<T>, WorkloadError> {
        ListOptions::new(options.max_pages, options.max_items, options.max_bytes)?;
        let request = self.page_request::<T>(
            None,
            options.refresh_binding_resolution,
            &options.rule_entities,
        )?;
        self.collect(request, options).await
    }

    pub(crate) async fn collect<T: NativeModel>(
        &self,
        first: WorkloadRequest,
        options: &ListOptions,
    ) -> Result<ListResult<T>, WorkloadError> {
        let mut result = ListResult {
            items: Vec::new(),
            responses: Vec::new(),
            continuation_token: None,
            stop: ListStop::Limit,
        };
        let mut tokens = BTreeSet::new();
        let mut total_bytes = 0usize;
        for index in 0..options.max_pages {
            let request = (|| {
                let mut request = first.clone();
                if let Some(token) = &result.continuation_token {
                    request = request.with_continuation_token(token)?;
                }
                request
                    .with_response_bound(self.limits.response.min(options.max_bytes - total_bytes))
            })();
            let request = match request {
                Ok(request) => request,
                Err(error) => {
                    result.stop = ListStop::Failed(error);
                    return Ok(result);
                }
            };
            match self.page_result::<T>(&request).await {
                Ok(Outcome::Accepted { response }) => {
                    result.responses.push(response);
                    result.stop = ListStop::Accepted;
                    return Ok(result);
                }
                Err(error) => {
                    if let Some(response) = &error.response {
                        result.responses.push(response.clone());
                    }
                    result.stop = ListStop::Failed(error);
                    return Ok(result);
                }
                Ok(Outcome::Completed {
                    value: page,
                    response,
                }) => {
                    total_bytes += response.body.len();
                    result.responses.push(response);
                    if page.value.len() > options.max_items - result.items.len() {
                        result.stop = ListStop::Limit;
                        return Ok(result);
                    }
                    if page
                        .continuation_token
                        .as_ref()
                        .is_some_and(|token| !tokens.insert(token.clone()))
                    {
                        let error = WorkloadError::with_response(
                            WorkloadErrorKind::Protocol("repeated continuation token"),
                            result.responses.last().expect("observed page").clone(),
                            DispatchState::Completed,
                        );
                        result.stop = ListStop::Failed(error);
                        return Ok(result);
                    }
                    result.items.extend(page.value);
                    result.continuation_token = page.continuation_token;
                    if result.continuation_token.is_none() {
                        result.stop = ListStop::Complete;
                        return Ok(result);
                    }
                    if result.items.len() == options.max_items
                        || total_bytes == options.max_bytes
                        || index + 1 == options.max_pages
                    {
                        return Ok(result);
                    }
                }
            }
        }
        Ok(result)
    }

    pub(crate) async fn resource_result<T: NativeModel>(
        &self,
        request: &WorkloadRequest,
        expected: u16,
    ) -> Result<Outcome<NativeResource<T>>, WorkloadError> {
        self.json_result(request, expected, |value| NativeResource::decode(value))
            .await
    }

    pub(crate) async fn page_result<T: NativeModel>(
        &self,
        request: &WorkloadRequest,
    ) -> Result<Outcome<NativePage<T>>, WorkloadError> {
        self.json_result(request, 200, decode_page).await
    }

    pub(crate) async fn json_result<T>(
        &self,
        request: &WorkloadRequest,
        expected: u16,
        decode: impl FnOnce(Value) -> Result<T, WorkloadError>,
    ) -> Result<Outcome<T>, WorkloadError> {
        let response = execute_once(self.executor, request).await?;
        if response.status == 202 {
            return Ok(Outcome::Accepted { response });
        }
        expect_status(&response, expected, request.method())?;
        let media = response
            .header("Content-Type")?
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !media.eq_ignore_ascii_case("application/json") {
            return Err(protocol(request, response, "expected application/json"));
        }
        let decoded = serde_json::from_slice::<Value>(&response.body)
            .map_err(|_| protocol(request, response.clone(), "invalid response JSON"))?;
        let value = decode(decoded)
            .map_err(|_| protocol(request, response.clone(), "invalid native response schema"))?;
        Ok(Outcome::Completed { value, response })
    }

    pub(crate) async fn empty_result(
        &self,
        request: &WorkloadRequest,
    ) -> Result<Outcome<()>, WorkloadError> {
        let response = execute_once(self.executor, request).await?;
        if response.status == 202 {
            return Ok(Outcome::Accepted { response });
        }
        expect_status(&response, 204, request.method())?;
        if !response.body.is_empty() {
            return Err(protocol(request, response, "204 response has a body"));
        }
        Ok(Outcome::Completed {
            value: (),
            response,
        })
    }
}

pub(crate) fn to_value(value: &impl Serialize) -> Result<Value, WorkloadError> {
    serde_json::to_value(value).map_err(|_| WorkloadError::invalid("native serialization"))
}
pub(crate) fn to_bytes(value: &impl Serialize, bound: usize) -> Result<Vec<u8>, WorkloadError> {
    struct BoundedWriter {
        bytes: Vec<u8>,
        bound: usize,
        exceeded: bool,
    }
    impl std::io::Write for BoundedWriter {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            if input.len() > self.bound.saturating_sub(self.bytes.len()) {
                self.exceeded = true;
                return Err(std::io::Error::other("request byte bound"));
            }
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        bound,
        exceeded: false,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        if writer.exceeded {
            return Err(WorkloadError {
                kind: WorkloadErrorKind::Limit,
                dispatch: DispatchState::NotSent,
                response: None,
                detail: None,
            });
        }
        return Err(WorkloadError::invalid("native serialization"));
    }
    Ok(writer.bytes)
}
pub(crate) fn validate_token(token: &str) -> Result<(), WorkloadError> {
    if token.is_empty() || token.len() > 4096 || token.chars().any(char::is_control) {
        return Err(WorkloadError::invalid("continuation token"));
    }
    Ok(())
}
pub(crate) fn decode_page<T: NativeModel>(value: Value) -> Result<NativePage<T>, WorkloadError> {
    let map = value
        .as_object()
        .ok_or_else(|| WorkloadError::invalid("page envelope"))?;
    let values = map
        .get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| WorkloadError::invalid("required page value array"))?;
    let token = match map.get("continuationToken") {
        None | Some(Value::Null) => None,
        Some(Value::String(token)) => {
            validate_token(token)?;
            Some(token.clone())
        }
        _ => return Err(WorkloadError::invalid("page token type")),
    };
    if token.is_none() && map.get("continuationUri").is_some_and(|uri| !uri.is_null()) {
        return Err(WorkloadError::invalid(
            "continuationUri without reusable same-route token",
        ));
    }
    let resources = values
        .iter()
        .cloned()
        .map(NativeResource::decode)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NativePage {
        value: resources,
        continuation_token: token,
        raw: value,
    })
}

pub(crate) fn expect_status(
    response: &WorkloadResponse,
    expected: u16,
    method: WorkloadMethod,
) -> Result<(), WorkloadError> {
    if response.status == expected {
        return Ok(());
    }
    let kind = if !(200..300).contains(&response.status) {
        WorkloadErrorKind::Http
    } else {
        WorkloadErrorKind::Protocol("unexpected endpoint status")
    };
    Err(WorkloadError::with_response(
        kind,
        response.clone(),
        if method == WorkloadMethod::Get {
            DispatchState::Completed
        } else {
            DispatchState::Unknown
        },
    ))
}

pub(crate) fn protocol(
    request: &WorkloadRequest,
    response: WorkloadResponse,
    reason: &'static str,
) -> WorkloadError {
    WorkloadError::with_response(
        WorkloadErrorKind::Protocol(reason),
        response,
        if request.method() == WorkloadMethod::Get {
            DispatchState::Completed
        } else {
            DispatchState::Unknown
        },
    )
}
