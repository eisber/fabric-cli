// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source hidden-resource and OneLake GET composition over released SDK policies.

use super::{
    ClockedGetHost, GetDispatch, GetEnrichmentFailure, GetErrorKind, GetEvidence, RuntimeGetHost,
    checkpoint, finish, operation, parse_object, project_and_output, render,
};
use crate::session::snapshot::Snapshot;
use crate::{PythonQuery, ReadHost, errors, parser::ReadCommand, session::ReadSession};
use azure_core::http::Method;
use fabric_cli_context::{
    ContextError, Element, HierarchyLookup, Lookup, LookupKind, LookupRequest, NodeKind,
    OneLakeKind, OneLakeProperties, PathElement, PathInput, PathKind, PathResolver, ResolveOptions,
    VirtualItemKind, VirtualLookupRequest, VirtualWorkspaceKind,
};
use fabric_cli_output::OutputInput;
use fabric_cli_transfer::TransferEvent;
use fabric_sdk::{Audience, CredentialSelection};
use fabric_sdk_api::{
    ApiClient, ApiError, Limits, Origin, Paging, RawRequestOptions, Request, RequestBounds,
};
use fabric_sdk_catalog::{CatalogClient, HiddenCollection, HiddenResource};
use fabric_sdk_http::{HttpExecutor, PublicOrigin};
use fabric_sdk_onelake::{OneLakeError, OneLakeLimits, OneLakePath};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_workspaces::mutations::{ResourceMethod, ResourceOutcome};
use futures_util::{FutureExt, future::FusedFuture};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::Future,
    pin::{Pin, pin},
    sync::Arc,
};

pub use super::GetResourceConfig;

enum Needed {
    Fabric(Arc<Element>, LookupKind),
    Virtual(Arc<PathElement>, PathKind, String),
    Properties(String),
}

#[derive(Default)]
struct Context {
    snapshot: Snapshot,
    virtual_ids: BTreeMap<String, Option<String>>,
    virtual_rows: BTreeMap<String, Vec<Value>>,
    properties: BTreeMap<String, Option<OneLakeProperties>>,
    external_items: BTreeMap<String, String>,
}

fn virtual_key(request: VirtualLookupRequest<'_>) -> String {
    format!("{}/{}", request.parent.path(), request.name)
}

struct Loading<'a> {
    context: &'a Context,
    needed: &'a RefCell<Option<Needed>>,
}

fn required() -> ContextError {
    ContextError::lookup("SnapshotRequired", "Metadata required")
}

impl Lookup for Loading<'_> {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        if !self.context.snapshot.ready(request) {
            self.needed
                .replace(Some(Needed::Fabric(request.parent.clone(), request.kind)));
            return Err(required());
        }
        self.context.snapshot.lookup(request)
    }
}

impl HierarchyLookup for Loading<'_> {
    fn virtual_id(
        &self,
        request: VirtualLookupRequest<'_>,
    ) -> Result<Option<String>, ContextError> {
        let key = virtual_key(request);
        match self.context.virtual_ids.get(&key) {
            Some(id) => Ok(id.clone()),
            None => {
                self.needed.replace(Some(Needed::Virtual(
                    request.parent.clone(),
                    request.kind,
                    request.name.into(),
                )));
                Err(required())
            }
        }
    }

    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        match self.context.properties.get(path) {
            Some(properties) => Ok(properties.clone()),
            None => {
                self.needed.replace(Some(Needed::Properties(path.into())));
                Err(required())
            }
        }
    }
}

impl Context {
    async fn resolve(
        &mut self,
        reads: &ReadSession<'_>,
        http: &HttpExecutor,
        current: Arc<PathElement>,
        path: &str,
        host: &RuntimeGetHost<'_>,
        mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
    ) -> Result<Arc<PathElement>, GetErrorKind> {
        for _ in 0..128 {
            checkpoint(cancelled.as_mut()).await?;
            let (result, needed) = {
                let needed = RefCell::new(None);
                let lookup = Loading {
                    context: self,
                    needed: &needed,
                };
                let resolver =
                    PathResolver::new(&lookup, current.clone(), reads.home.clone(), "ls")
                        .map_err(context_error)?;
                let result = resolver.resolve(PathInput::Scalar(path), ResolveOptions::default());
                (result, needed.into_inner())
            };
            match needed {
                None => {
                    let result = result.map_err(|failure| context_error(failure.error))?;
                    for notice in result.notices {
                        host.ui
                            .emit(TransferEvent::Warning(notice.message().into()))?;
                    }
                    return Ok(result.element);
                }
                Some(Needed::Fabric(parent, kind)) => {
                    self.snapshot
                        .load_lookup(reads, parent, kind, cancelled.as_mut())
                        .await?;
                }
                Some(Needed::Virtual(parent, kind, name)) => {
                    self.load_virtual(reads, http, &parent, kind, &name, cancelled.as_mut())
                        .await?;
                }
                Some(Needed::Properties(path)) => {
                    let target = storage_path(&path)?;
                    let result = crate::families::storage_client(reads)
                        .properties(
                            &OneLakeHttpExecutor::new(Arc::new(http.clone())),
                            &target,
                            OneLakeLimits::default(),
                            cancelled.as_mut(),
                        )
                        .await;
                    let properties = match result {
                        Ok(headers) => {
                            let header = |name: &str| {
                                headers
                                    .iter()
                                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                                    .map(|(_, value)| value.clone())
                            };
                            Some(OneLakeProperties {
                                status_code: 200,
                                resource_type: header("x-ms-resource-type"),
                                is_shortcut: header("x-ms-onelake-shortcut-path").as_deref()
                                    == Some("true"),
                            })
                        }
                        Err(OneLakeError::Http { status: 404, .. }) => None,
                        Err(error) => return Err(error.into()),
                    };
                    self.properties.insert(path, properties);
                }
            }
        }
        Err(errors::unsupported("GET path exceeds the command metadata-scope bound").into())
    }

    async fn load_virtual(
        &mut self,
        reads: &ReadSession<'_>,
        http: &HttpExecutor,
        parent: &Arc<PathElement>,
        kind: PathKind,
        name: &str,
        mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
    ) -> Result<(), GetErrorKind> {
        let client = CatalogClient::new(api(reads, http, Audience::Fabric));
        let (collection, name_field, suffix) = match kind {
            PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Capacity) => {
                let capacities = reads
                    .sdk
                    .list_capacities(reads.executor, Default::default(), cancelled.as_mut())
                    .await
                    .map_err(|error| errors::capacity(&error.kind))?;
                let id = capacities
                    .value
                    .into_iter()
                    .find(|row| format!("{}.Capacity", row.display_name) == name)
                    .map(|row| row.id);
                self.virtual_ids
                    .insert(format!("{}/{}", parent.path(), name), id);
                return Ok(());
            }
            PathKind::VirtualWorkspaceItem(kind) => (
                match kind {
                    VirtualWorkspaceKind::Connection => HiddenCollection::Connections,
                    VirtualWorkspaceKind::Gateway => HiddenCollection::Gateways,
                    VirtualWorkspaceKind::Domain => HiddenCollection::Domains,
                    VirtualWorkspaceKind::Capacity => unreachable!(),
                },
                "displayName",
                kind.suffix(),
            ),
            PathKind::VirtualItem(kind) => {
                let workspace = ancestor(parent, |kind| {
                    matches!(kind, PathKind::Fabric(NodeKind::Workspace(_)))
                })?;
                let ws = id(&workspace)?;
                let collection = match kind {
                    VirtualItemKind::SparkPool => HiddenCollection::SparkPools(ws.into()),
                    VirtualItemKind::ManagedPrivateEndpoint => {
                        HiddenCollection::ManagedPrivateEndpoints(ws.into())
                    }
                    VirtualItemKind::ManagedIdentity => {
                        return Err(errors::error(
                            "UnsupportedCommand",
                            "not supported for command 'get'",
                        )
                        .into());
                    }
                    VirtualItemKind::ExternalDataShare => {
                        let short = name
                            .strip_suffix(".ExternalDataShare")
                            .ok_or(ApiError::InvalidInput)?;
                        let (item_name, _) =
                            short.rsplit_once('_').ok_or(ApiError::InvalidInput)?;
                        let fabric = workspace.fabric().ok_or(ApiError::InvalidInput)?;
                        let item = self
                            .snapshot
                            .resolve(reads, fabric, item_name, cancelled.as_mut())
                            .await?;
                        let item_id = crate::reads::required_id(&item)?.to_owned();
                        let key = format!("{}/{}", parent.path(), name);
                        self.external_items.insert(key, item_id.clone());
                        HiddenCollection::ItemExternalDataShares {
                            workspace: ws.into(),
                            item: item_id,
                        }
                    }
                };
                (collection, "name", kind.suffix())
            }
            _ => return Err(ApiError::InvalidInput.into()),
        };
        let scope = collection.request()?.0.url().to_string();
        if !self.virtual_rows.contains_key(&scope) {
            let rows = client
                .list_hidden(&collection, Limits::default(), cancelled.as_mut())
                .await?;
            self.virtual_rows.insert(scope.clone(), rows.rows);
        }
        let rows = self
            .virtual_rows
            .get(&scope)
            .expect("loaded complete scope");
        let mut found = None;
        for row in rows {
            let row_id = row
                .get("id")
                .and_then(Value::as_str)
                .ok_or(ApiError::InvalidResponse)?;
            let full_name = if let HiddenCollection::ItemExternalDataShares { .. } = collection {
                let short = name
                    .strip_suffix(".ExternalDataShare")
                    .ok_or(ApiError::InvalidInput)?;
                let (item_name, _) = short.rsplit_once('_').ok_or(ApiError::InvalidInput)?;
                format!(
                    "{item_name}_{}.ExternalDataShare",
                    row_id.split('-').next().unwrap_or("")
                )
            } else {
                let display = match &collection {
                    HiddenCollection::Connections => {
                        let value = row.get("displayName").ok_or(ApiError::InvalidResponse)?;
                        if super::truthy(value) {
                            value.as_str().ok_or(ApiError::InvalidResponse)?
                        } else {
                            row_id
                        }
                    }
                    HiddenCollection::Gateways
                        if !row
                            .as_object()
                            .is_some_and(|row| row.contains_key("displayName")) =>
                    {
                        row_id
                    }
                    _ => row
                        .get(name_field)
                        .and_then(Value::as_str)
                        .ok_or(ApiError::InvalidResponse)?,
                };
                format!("{display}.{suffix}")
            };
            if full_name == name {
                found = (!row_id.is_empty()).then(|| row_id.to_owned());
                break;
            }
        }
        self.virtual_ids
            .insert(format!("{}/{}", parent.path(), name), found);
        Ok(())
    }
}

fn context_error(error: ContextError) -> GetErrorKind {
    errors::error(&error.code, error.message).into()
}

fn id(target: &PathElement) -> Result<&str, GetErrorKind> {
    target.id().filter(|id| !id.is_empty()).ok_or_else(|| {
        errors::error("NotFound", "The requested resource could not be found").into()
    })
}

fn ancestor(
    target: &Arc<PathElement>,
    matches: impl Fn(PathKind) -> bool,
) -> Result<Arc<PathElement>, GetErrorKind> {
    let mut node = target.clone();
    loop {
        if matches(node.kind()) {
            return Ok(node);
        }
        node = node.parent().cloned().ok_or(ApiError::InvalidInput)?;
    }
}

fn storage_path(path: &str) -> Result<OneLakePath, GetErrorKind> {
    let mut parts = path.splitn(3, '/');
    Ok(OneLakePath::new(
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    )?)
}

fn api(reads: &ReadSession<'_>, http: &HttpExecutor, audience: Audience) -> ApiClient {
    ApiClient::new(
        reads.sdk.credential().clone(),
        CredentialSelection::new(
            reads.sdk.selection().account(),
            reads.sdk.selection().tenant(),
            audience,
        )
        .expect("validated existing account and tenant"),
        Arc::new(BoundResources {
            http: http.clone(),
            fabric_host: reads.sdk.context().endpoint().host().into(),
        }),
    )
}

struct BoundResources {
    http: HttpExecutor,
    fabric_host: String,
}

fn public_origin(origin: Origin) -> PublicOrigin {
    match origin {
        Origin::Fabric => PublicOrigin::Fabric,
        Origin::Arm => PublicOrigin::Arm,
        Origin::OneLake => PublicOrigin::OneLake,
        Origin::PowerBi => PublicOrigin::PowerBi,
        Origin::RelevanceEngine => PublicOrigin::RelevanceEngine,
    }
}

impl fabric_sdk_api::Executor for BoundResources {
    fn supports(&self, _: &Request) -> bool {
        true
    }

    fn wait(
        &self,
        delay: std::time::Duration,
    ) -> fabric_sdk_http::ExecutionFuture<'_, Result<(), fabric_sdk_api::ExecutionError>> {
        Box::pin(async move { self.http.wait_for(delay).await.map_err(api_execution) })
    }

    fn execute<'a>(
        &'a self,
        request: &'a Request,
        token: &'a azure_core::credentials::AccessToken,
        limit: std::num::NonZeroUsize,
    ) -> fabric_sdk_http::ExecutionFuture<
        'a,
        Result<fabric_sdk_api::Response, fabric_sdk_api::ExecutionError>,
    > {
        Box::pin(async move {
            let mut url = request.url().clone();
            let origin = public_origin(request.origin());
            if request.origin() == Origin::Fabric {
                url.set_host(Some(&self.fabric_host))
                    .map_err(|_| fabric_sdk_api::ExecutionError::Failed)?;
            }
            self.http
                .execute_bound(
                    origin,
                    request.method(),
                    &url,
                    token,
                    request.body(),
                    request.headers(),
                    limit,
                )
                .await
                .map_err(api_execution)
        })
    }
}

fn api_execution(error: fabric_sdk_http::ExecutionError) -> fabric_sdk_api::ExecutionError {
    match error {
        fabric_sdk_http::ExecutionError::TimedOut => fabric_sdk_api::ExecutionError::TimedOut,
        fabric_sdk_http::ExecutionError::ResponseLimit => {
            fabric_sdk_api::ExecutionError::ResponseLimit
        }
        fabric_sdk_http::ExecutionError::RetriesExhausted => {
            fabric_sdk_api::ExecutionError::RetriesExhausted
        }
        _ => fabric_sdk_api::ExecutionError::Failed,
    }
}

fn collection_failure(
    error: fabric_sdk_api::CollectionError,
    evidence: &mut GetEvidence,
    phase: &'static str,
) -> GetErrorKind {
    if let ApiError::Operation(operation) = &error.error {
        evidence.operations.push(super::GetOperationReceipt {
            phase,
            operation_id: operation.operation_id.clone(),
            mutation_may_have_occurred: operation.mutation_may_have_occurred,
        });
    }
    error.into()
}

impl ReadSession<'_> {
    /// Resolve all source GET resource kinds using existing SDK and HTTP policies.
    ///
    /// Configuration, file authority and UI remain caller-owned. Ordinary Fabric
    /// targets use the unchanged definition engine, including its effect receipts.
    pub async fn dispatch_get_with_resources(
        &self,
        command: &ReadCommand,
        http: &HttpExecutor,
        host: &mut ClockedGetHost<'_>,
        evidence: &mut GetEvidence,
        config: &GetResourceConfig,
        cancelled: impl Future<Output = ()> + Send,
    ) -> GetDispatch {
        let mut cancelled = pin!(cancelled.fuse());
        let mut host = host.borrowed();
        let result = self
            .execute_resources(
                command,
                http,
                &mut host,
                evidence,
                config,
                cancelled.as_mut(),
            )
            .await;
        render(
            finish(result, std::mem::take(evidence)),
            command,
            self.timestamp.clone(),
        )
    }

    async fn execute_resources(
        &self,
        command: &ReadCommand,
        http: &HttpExecutor,
        host: &mut RuntimeGetHost<'_>,
        evidence: &mut GetEvidence,
        config: &GetResourceConfig,
        mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
    ) -> Result<(Option<Value>, Option<OutputInput>), GetErrorKind> {
        crate::session::get_options::source_preflight(command)?;
        if self.tenant.id() != Some(self.sdk.selection().tenant())
            || self.tenant.kind() != NodeKind::Tenant
            || self.sdk.selection().audience() != Audience::Fabric
            || !self.current_path.starts_with('/')
        {
            return Err(errors::error("InvalidInput", "Invalid selected read context").into());
        }
        let mut context = Context::default();
        let root = PathElement::from_fabric(self.tenant.clone());
        let path = command.path.join(" ");
        let path = path.trim_matches(['\'', '"']);
        let current = if path.starts_with('/') || path == "~" || path.starts_with("~/") {
            root
        } else {
            context
                .resolve(
                    self,
                    http,
                    root,
                    self.current_path,
                    host,
                    cancelled.as_mut(),
                )
                .await?
        };
        let target = context
            .resolve(self, http, current, path, host, cancelled.as_mut())
            .await?;
        if let Some(fabric) = target.fabric() {
            let reads = ReadHost {
                sdk: self.sdk,
                workspaces: self.executor,
                items: self.executor,
                capacities: self.executor,
                lookup: &context.snapshot,
                current: self.tenant.clone(),
                home: self.home.clone(),
                timestamp: self.timestamp.clone(),
                config: self.config,
                projection: &PythonQuery,
            };
            return reads
                .source_get_resolved(command, fabric, host, cancelled.as_mut(), evidence)
                .await;
        }
        let mut output_name = target.name();
        let data = match target.kind() {
            PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Capacity) => {
                capacity(
                    self,
                    http,
                    &target,
                    config,
                    host,
                    evidence,
                    cancelled.as_mut(),
                )
                .await?
            }
            PathKind::VirtualWorkspaceItem(kind) => {
                let resource = match kind {
                    VirtualWorkspaceKind::Connection => {
                        HiddenResource::Connection(id(&target)?.into())
                    }
                    VirtualWorkspaceKind::Gateway => HiddenResource::Gateway(id(&target)?.into()),
                    VirtualWorkspaceKind::Domain => {
                        host.ui.emit(TransferEvent::Warning(
                            "Requires Fabric administrator".into(),
                        ))?;
                        HiddenResource::Domain(id(&target)?.into())
                    }
                    VirtualWorkspaceKind::Capacity => unreachable!(),
                };
                hidden(self, http, host, resource, evidence, cancelled.as_mut()).await?
            }
            PathKind::VirtualItem(kind) => {
                let workspace = ancestor(&target, |kind| {
                    matches!(kind, PathKind::Fabric(NodeKind::Workspace(_)))
                })?;
                let workspace = id(&workspace)?.to_owned();
                let resource = match kind {
                    VirtualItemKind::SparkPool => HiddenResource::SparkPool {
                        workspace,
                        id: id(&target)?.into(),
                    },
                    VirtualItemKind::ManagedPrivateEndpoint => {
                        HiddenResource::ManagedPrivateEndpoint {
                            workspace,
                            id: id(&target)?.into(),
                        }
                    }
                    VirtualItemKind::ExternalDataShare => HiddenResource::ExternalDataShare {
                        workspace,
                        item: context
                            .external_items
                            .get(&target.path())
                            .cloned()
                            .ok_or(ApiError::InvalidInput)?,
                        id: id(&target)?.into(),
                    },
                    VirtualItemKind::ManagedIdentity => {
                        return Err(errors::error(
                            "UnsupportedCommand",
                            "not supported for command 'get'",
                        )
                        .into());
                    }
                };
                hidden(self, http, host, resource, evidence, cancelled.as_mut()).await?
            }
            PathKind::OneLake(OneLakeKind::Shortcut) => {
                let local = target.local_path().ok_or(ApiError::InvalidInput)?;
                let (path, name) = local.rsplit_once('/').ok_or(ApiError::InvalidInput)?;
                let item = ancestor(&target, |kind| {
                    matches!(kind, PathKind::Fabric(NodeKind::Item(_)))
                })?;
                let workspace = ancestor(&item, |kind| {
                    matches!(kind, PathKind::Fabric(NodeKind::Workspace(_)))
                })?;
                let shortcut = fabric_sdk_onelake::shortcuts::ShortcutPath::new(
                    id(&workspace)?,
                    id(&item)?,
                    path,
                    name,
                )
                .map_err(|error| errors::error("InvalidInput", error.to_string()))?;
                let mut observed = super::TrackedOperation {
                    session: host.operation_session(self.sdk, cancelled.as_mut()),
                    evidence,
                    phase: "shortcut",
                    post: false,
                };
                let result = fabric_sdk_onelake::shortcuts::ShortcutMutations::get(
                    &mut observed.session,
                    &shortcut,
                )
                .await
                .map_err(|error| GetErrorKind::Resource(Box::new(error)))?;
                match result {
                    ResourceOutcome::Completed { status: 200, data } => Some(
                        data.filter(Value::is_object)
                            .ok_or(ApiError::InvalidResponse)?,
                    ),
                    ResourceOutcome::Completed { status, .. } => {
                        return Err(ApiError::UnsupportedStatus(status).into());
                    }
                    ResourceOutcome::Accepted { .. } => {
                        return Err(ApiError::UnsupportedStatus(202).into());
                    }
                }
            }
            PathKind::OneLake(_) => {
                output_name = target.local_path().ok_or(ApiError::InvalidInput)?;
                let path = storage_path(target.path_id().trim_start_matches('/'))?;
                let mut metadata = crate::families::storage_client(self)
                    .directory_metadata(
                        &OneLakeHttpExecutor::new(Arc::new(http.clone())),
                        &path,
                        OneLakeLimits::default(),
                        cancelled.as_mut(),
                    )
                    .await?;
                metadata
                    .data
                    .as_object_mut()
                    .ok_or(ApiError::InvalidResponse)?
                    .remove("ContinuationToken");
                Some(metadata.data)
            }
            _ => {
                return Err(
                    errors::error("UnsupportedCommand", "not supported for command 'get'").into(),
                );
            }
        };
        let Some(data) = data else {
            return Ok((None, None));
        };
        project_and_output(
            self.sdk,
            &PythonQuery,
            command,
            &output_name,
            data,
            host,
            evidence,
            cancelled,
        )
        .await
    }
}

async fn hidden(
    reads: &ReadSession<'_>,
    http: &HttpExecutor,
    host: &RuntimeGetHost<'_>,
    resource: HiddenResource,
    evidence: &mut GetEvidence,
    mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
) -> Result<Option<Value>, GetErrorKind> {
    let request = resource.request()?;
    let response = operation(
        reads.sdk,
        host,
        "hidden resource",
        ResourceMethod::Get,
        request.url().path(),
        evidence,
        cancelled.as_mut(),
    )
    .await?;
    if response.status != 200 {
        return Ok(None);
    }
    let mut data = parse_object(&response)?;
    if let HiddenResource::Domain(id) = resource {
        let client = CatalogClient::new(api(reads, http, Audience::Fabric));
        match client
            .list_hidden(
                &HiddenCollection::DomainWorkspaces(id),
                Limits::default(),
                cancelled.as_mut(),
            )
            .await
        {
            Ok(workspaces) => data["domainWorkspaces"] = Value::Array(workspaces.rows),
            Err(error)
                if matches!(
                    error.error,
                    ApiError::Http { .. }
                        | ApiError::Service { .. }
                        | ApiError::InvalidJson
                        | ApiError::InvalidResponse
                        | ApiError::UnsupportedStatus(_)
                ) =>
            {
                evidence.enrichment_failures.push(GetEnrichmentFailure {
                    phase: "domain workspaces",
                    kind: error.into(),
                })
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(data))
}

fn arm_request(segments: &[&str], version: &str) -> Result<Request, ApiError> {
    let validated = Request::fabric(Method::Get, segments)?;
    Request::raw(RawRequestOptions {
        origin: Origin::Arm,
        method: Method::Get,
        endpoint: validated.url().path().trim_start_matches("/v1/").into(),
        query: vec![("api-version".into(), version.into())],
        headers: Vec::new(),
        body: Vec::new(),
        bounds: RequestBounds::default(),
    })
}

async fn arm_get(
    reads: &ReadSession<'_>,
    host: &RuntimeGetHost<'_>,
    request: &Request,
    evidence: &mut GetEvidence,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Option<Value>, GetErrorKind> {
    let selection = CredentialSelection::new(
        reads.sdk.selection().account(),
        reads.sdk.selection().tenant(),
        Audience::Arm,
    )
    .expect("validated existing account and tenant");
    let mut observed = super::TrackedOperation {
        session: fabric_sdk_operations::OperationSession::for_arm(
            reads.sdk.credential().as_ref(),
            &selection,
            super::BorrowedOperations(host.operations),
            host.operation_limits,
            cancelled,
        ),
        evidence,
        phase: "capacity",
        post: false,
    };
    let path = format!(
        "{}{}",
        request.url().path(),
        request
            .url()
            .query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()
    );
    let operation = fabric_sdk_operations::ArmGetOperation::new(&path)
        .map_err(|kind| observed.session.evidence(kind))?;
    let response = observed.session.execute_arm_get(operation).await?;
    if response.status != 200 {
        return Ok(None);
    }
    Ok(Some(parse_object(&response)?))
}

async fn capacity(
    reads: &ReadSession<'_>,
    http: &HttpExecutor,
    target: &PathElement,
    config: &GetResourceConfig,
    host: &RuntimeGetHost<'_>,
    evidence: &mut GetEvidence,
    mut cancelled: Pin<&mut (impl FusedFuture<Output = ()> + Send)>,
) -> Result<Option<Value>, GetErrorKind> {
    let fabric_id = id(target)?;
    let capacities = reads
        .sdk
        .list_capacities(reads.executor, Default::default(), cancelled.as_mut())
        .await
        .map_err(|error| errors::capacity(&error.kind))?;
    let capacity = capacities
        .value
        .iter()
        .find(|row| row.id == fabric_id)
        .ok_or_else(|| {
            errors::error("NotFound", format!("Capacity {} not found", target.name()))
        })?;
    // Python re.match(r"F\d+") is prefix-based, not a complete SKU validator.
    let sku = capacity.sku().and_then(Value::as_str).unwrap_or("Unknown");
    let unicode = fabric_cli_output::production_unicode::embedded_provider().map_err(|_| {
        errors::error(
            "UnicodeDataUnavailable",
            "Source Unicode data is unavailable",
        )
    })?;
    if !sku.strip_prefix('F').is_some_and(|tail| {
        tail.chars()
            .next()
            .is_some_and(|c| unicode.properties(c).general_category == "Nd")
    }) {
        return Err(errors::error(
            "Not a Fabric capacity",
            format!(
                "Capacity {} is not a F SKU. Only Fabric capacities are supported",
                target.name()
            ),
        )
        .into());
    }
    let client = api(reads, http, Audience::Arm).with_arm_get_reconciliation(host.operation_limits);
    let mut resource_id = None;
    if let (Some(subscription), Some(group)) = (
        config.subscription_id.as_deref().filter(|s| !s.is_empty()),
        config.resource_group.as_deref().filter(|s| !s.is_empty()),
    ) {
        let request = arm_request(
            &[
                "subscriptions",
                subscription,
                "resourceGroups",
                group,
                "providers",
                "Microsoft.Fabric",
                "capacities",
                target.short_name(),
            ],
            "2023-11-01",
        )?;
        match arm_get(reads, host, &request, evidence, cancelled.as_mut()).await {
            Ok(Some(value)) => {
                resource_id = Some(
                    value
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or(ApiError::InvalidResponse)?
                        .to_owned(),
                )
            }
            Ok(None) => {}
            Err(error)
                if matches!(&error, GetErrorKind::Operation(operation)
                if matches!(&operation.kind, fabric_sdk_operations::OperationErrorKind::Http { .. })
                    || matches!(&operation.kind,
                        fabric_sdk_operations::OperationErrorKind::OperationFailed { status, .. } if status == "Failed")) =>
            {
                evidence.enrichment_failures.push(GetEnrichmentFailure {
                    phase: "configured capacity lookup",
                    kind: error,
                });
            }
            Err(error) => return Err(error),
        }
    }
    if resource_id.is_none() {
        let subscriptions = client
            .collect(
                arm_request(&["subscriptions"], "2022-12-01")?,
                "value",
                Paging::Query,
                Limits::default(),
                cancelled.as_mut(),
            )
            .await
            .map_err(|error| collection_failure(error, evidence, "capacity subscriptions"))?;
        for subscription in subscriptions.rows {
            let subscription = subscription
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| id.rsplit('/').next())
                .ok_or(ApiError::InvalidResponse)?;
            let capacities = client
                .collect(
                    arm_request(
                        &[
                            "subscriptions",
                            subscription,
                            "providers",
                            "Microsoft.Fabric",
                            "capacities",
                        ],
                        "2023-11-01",
                    )?,
                    "value",
                    Paging::Query,
                    Limits::default(),
                    cancelled.as_mut(),
                )
                .await
                .map_err(|error| collection_failure(error, evidence, "capacity collection"))?;
            for capacity in capacities.rows {
                let name = capacity
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(ApiError::InvalidResponse)?;
                if name.to_lowercase() == target.short_name().to_lowercase() {
                    resource_id = Some(
                        capacity
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or(ApiError::InvalidResponse)?
                            .to_owned(),
                    );
                    break;
                }
            }
            if resource_id.is_some() {
                break;
            }
        }
    }
    let resource_id = resource_id.ok_or_else(|| errors::error("NotFound",
        "Capacity not found or insufficient permissions. Try to set the Azure subscription ID and resource group name using 'config set default_az_subscription_id <subscription_id>' and 'config set default_az_resource_group <rg_name>'"))?;
    let parts: Vec<_> = resource_id.split('/').collect();
    if parts.len() != 9
        || parts[1] != "subscriptions"
        || parts[3] != "resourceGroups"
        || parts[5] != "providers"
        || !parts[6].eq_ignore_ascii_case("Microsoft.Fabric")
        || parts[7] != "capacities"
    {
        return Err(ApiError::InvalidResponse.into());
    }
    let Some(mut data) = arm_get(
        reads,
        host,
        &arm_request(
            &[
                "subscriptions",
                parts[2],
                "resourceGroups",
                parts[4],
                "providers",
                "Microsoft.Fabric",
                "capacities",
                target.short_name(),
            ],
            "2023-11-01",
        )?,
        evidence,
        cancelled.as_mut(),
    )
    .await?
    else {
        return Ok(None);
    };
    data["fabricId"] = Value::String(fabric_id.into());
    Ok(Some(data))
}

#[cfg(test)]
mod origin_tests {
    #![deny(unnameable_test_items)]

    use super::*;

    #[test]
    fn every_api_origin_retains_its_exact_transport_binding() {
        for (api, transport) in [
            (Origin::Fabric, PublicOrigin::Fabric),
            (Origin::Arm, PublicOrigin::Arm),
            (Origin::OneLake, PublicOrigin::OneLake),
            (Origin::PowerBi, PublicOrigin::PowerBi),
            (Origin::RelevanceEngine, PublicOrigin::RelevanceEngine),
        ] {
            assert_eq!(public_origin(api), transport);
            assert_eq!(api.audience(), transport.audience());
        }
    }
}
