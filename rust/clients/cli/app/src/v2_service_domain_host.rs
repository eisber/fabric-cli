// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in, bounded admin Domain resolution and immediate context-bound sync.

use crate::get_target::is_id;
use fabric_cli_resources::service::{
    self as command, DomainResolver, ExecuteError, ResolveFuture, SyncCommand,
    service::{
        self, Audience, DomainTarget, InputError, OperationLimits, RequestContext, ServiceOrigin,
        SyncReport,
        v2::{
            ApprovalPolicy, PreparedOperation, Registry, ResolvedTarget, Target, TargetKind,
            TenantIdentity,
        },
    },
};
use fabric_sdk_api::{ApiClient, ApiError, require_status};
use fabric_sdk_catalog::{HiddenCollection, HiddenResource};
use fabric_sdk_http::HttpExecutor;
use serde_json::Value;
use std::{convert::Infallible, future::Future, num::NonZeroUsize, pin::Pin, sync::Arc};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_DOMAINS: usize = 1000;

/// Local or bounded admin-read failure; provider errors remain typed, not printed.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Invalid typed path, tenant, audience or endpoint, before credentials or HTTP.
    #[error("Domain sync requires an absolute Domain path and explicit Fabric tenant context")]
    Context,
    /// Admin API rejection, cancellation, response limits or invalid JSON.
    #[error("Domain administrator metadata could not be verified")]
    Read(#[from] ApiError),
    /// Incomplete, ambiguous or inconsistent metadata is not authority to mutate.
    #[error("Domain metadata did not establish one exact Domain identity")]
    Identity,
    /// Shared operation registration/preparation failed.
    #[error("Domain sync operation preparation failed")]
    Preparation,
}

/// Validate the existing typed path grammar without credentials or discovery.
pub fn domain_selector(path: &str) -> Result<&str, ResolveError> {
    let selector = path
        .strip_prefix("/.domains/")
        .and_then(|value| value.strip_suffix(".Domain"))
        .filter(|value| {
            !value.trim().is_empty()
                && !matches!(*value, "." | "..")
                && !value.contains(['/', '\\', '?', '#', '%'])
                && !value.chars().any(char::is_control)
        })
        .ok_or(ResolveError::Context)?;
    if path.len() > 4096 {
        return Err(ResolveError::Context);
    }
    Ok(selector)
}

/// Admin metadata resolver with one bounded catalog response and one singleton.
///
/// Successful admin reads establish observed resource authority independently of
/// OAuth scope names. The mutation service still enforces current authorization.
/// There is no continuation traversal, cross-tenant search or name fallback.
pub struct BoundedDomainResolver<'a> {
    context: &'a RequestContext,
    executor: Arc<HttpExecutor>,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
}

impl<'a> BoundedDomainResolver<'a> {
    /// Bind one explicit caller context, matching executor and overall cancellation.
    pub fn new(
        context: &'a RequestContext,
        executor: Arc<HttpExecutor>,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> Result<Self, ResolveError> {
        if !is_id(context.selection().tenant())
            || context.selection().audience() != Audience::Fabric
            || context.endpoint().origin() != ServiceOrigin::Fabric
            || executor.endpoint() != context.endpoint()
        {
            return Err(ResolveError::Context);
        }
        Ok(Self {
            context,
            executor,
            cancelled: Box::pin(cancelled),
        })
    }

    async fn lookup(&mut self, path: &str) -> Result<DomainTarget, ResolveError> {
        let selector = domain_selector(path)?;
        let api = ApiClient::new(
            self.context.credential().clone(),
            self.context.selection().clone(),
            Arc::new(fabric_cli_resources::ResourceHttpExecutor(
                self.executor.clone(),
            )),
        );
        let limit = NonZeroUsize::new(MAX_RESPONSE_BYTES).expect("positive response bound");
        let id = if is_id(selector) {
            selector.to_owned()
        } else {
            let (request, field) = HiddenCollection::Domains.request()?;
            let response = api.send(&request, limit, self.cancelled.as_mut()).await?;
            let value = metadata(&response)?;
            let rows = value
                .get(field)
                .and_then(Value::as_array)
                .filter(|rows| rows.len() <= MAX_DOMAINS)
                .ok_or(ResolveError::Identity)?;
            let mut selected = None;
            for row in rows {
                let (id, name) = identity(row)?;
                if name == selector {
                    if selected.is_some() {
                        return Err(ResolveError::Identity);
                    }
                    selected = Some(id.to_owned());
                }
            }
            selected.ok_or(ResolveError::Identity)?
        };
        let request = HiddenResource::Domain(id.clone()).request()?;
        let response = api.send(&request, limit, self.cancelled.as_mut()).await?;
        let value = metadata(&response)?;
        let (verified_id, name) = identity(&value)?;
        if !verified_id.eq_ignore_ascii_case(&id) || (!is_id(selector) && name != selector) {
            return Err(ResolveError::Identity);
        }
        let target = ResolvedTarget::tenant_collection(
            path,
            TenantIdentity::new(self.context.selection().tenant())
                .map_err(|_| ResolveError::Context)?,
            Target {
                kind: TargetKind::TenantCollection,
                owner_type: "Domain".into(),
                facet: "roleAssignments".into(),
                representation: "native".into(),
                generation: None,
            },
        )
        .map_err(|_| ResolveError::Identity)?;
        DomainTarget::new(target, verified_id).map_err(|_| ResolveError::Identity)
    }
}

impl DomainResolver for BoundedDomainResolver<'_> {
    type Error = ResolveError;

    fn resolve<'a>(&'a mut self, path: &'a str) -> ResolveFuture<'a, Self::Error> {
        Box::pin(self.lookup(path))
    }
}

fn metadata(response: &fabric_sdk_api::Response) -> Result<Value, ResolveError> {
    require_status(response, &[200])?;
    if response.headers.iter().any(|(name, value)| {
        ["x-ms-continuation", "continuationtoken", "link"]
            .iter()
            .any(|expected| name.eq_ignore_ascii_case(expected))
            && !value.is_empty()
    }) {
        return Err(ResolveError::Identity);
    }
    let value: Value = serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidJson)?;
    for key in [
        "continuationToken",
        "continuationUri",
        "nextLink",
        "@odata.nextLink",
    ] {
        if let Some(continuation) = value.get(key)
            && !continuation.is_null()
            && continuation.as_str() != Some("")
        {
            return Err(ResolveError::Identity);
        }
    }
    Ok(value)
}

fn identity(value: &Value) -> Result<(&str, &str), ResolveError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| is_id(id))
        .ok_or(ResolveError::Identity)?;
    let name = value
        .get("displayName")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty() && !name.chars().any(char::is_control))
        .ok_or(ResolveError::Identity)?;
    Ok((id, name))
}

/// Single-use prepared action bound to the exact admin-read context and executor.
pub struct PreparedDomainSync {
    prepared: PreparedOperation,
    context: RequestContext,
    executor: Arc<HttpExecutor>,
}

impl PreparedDomainSync {
    /// Resolve and verify one Domain through the existing command preparation.
    ///
    /// Invalid roles, targets and context fail before credentials or sends.
    pub async fn prepare(
        command: &SyncCommand,
        context: &RequestContext,
        executor: Arc<HttpExecutor>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Self, ResolveError> {
        if command.role != "Contributor" {
            return Err(ResolveError::Preparation);
        }
        domain_selector(&command.path)?;
        let mut registry = Registry::default();
        service::register(&mut registry).map_err(|_| ResolveError::Preparation)?;
        let mut resolver = BoundedDomainResolver::new(context, executor.clone(), cancelled)?;
        let prepared = command::prepare(command, &registry, &mut resolver)
            .await
            .map_err(|error| match error {
                ExecuteError::Resolution(error) => error,
                _ => ResolveError::Preparation,
            })?;
        Ok(Self {
            prepared,
            context: context.clone(),
            executor,
        })
    }

    /// Exact operation, conditions and effects to present for immediate approval.
    pub fn operation(&self) -> &PreparedOperation {
        &self.prepared
    }

    /// Exact observed identity and endpoint; contains no token values.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Consume the action, reject context swaps, then immediately approve and send.
    ///
    /// The caller retains the original overall cancellation/deadline. No failure,
    /// cancellation, output loss or uncertain receipt authorizes replay.
    pub async fn execute<P: ApprovalPolicy>(
        self,
        context: &RequestContext,
        policy: &mut P,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<SyncReport, ExecuteError<Infallible, P::Error>> {
        if context.selection() != self.context.selection()
            || context.endpoint() != self.context.endpoint()
            || !Arc::ptr_eq(context.credential(), self.context.credential())
        {
            return Err(ExecuteError::Input(InputError::Context));
        }
        command::execute(
            &self.prepared,
            policy,
            context,
            (*self.executor).clone(),
            OperationLimits::default(),
            cancelled,
        )
        .await
    }
}
