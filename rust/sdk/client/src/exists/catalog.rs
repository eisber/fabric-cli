// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Lakehouse-owned Iceberg catalog checks, never physical directory probes.

use super::{
    ExistsError, ExistsHost, ExistsInterruption, ExistsLimits, ExistsObservation, ExistsReport,
    ExistsRequest, ExistsTarget,
    evidence::Evidence,
    probe::{self, Conclusion, ProbeError},
};
use crate::{Audience, EndpointBinding, ServiceOrigin};
use azure_core::http::Method;
use fabric_sdk_api::{
    ApiClient, ApiError, Origin, RawRequestOptions, Request, RequestBounds, Response, cancellable,
};
use fabric_v2_contracts::ApprovedOperation;
use serde_json::Value;
use std::{
    future::{Future, pending},
    num::NonZeroUsize,
    pin::pin,
};

/// Explicit Storage-scoped backend over caller-owned credentials/HTTP.
///
/// The endpoint's host is injected; the fixed API root is `/iceberg`.
/// Config properties never select a host, token, warehouse or authentication flow.
pub struct CatalogBackend {
    api: ApiClient,
    endpoint: EndpointBinding,
}

impl CatalogBackend {
    /// Validate the explicit endpoint/audience without credentials or network I/O.
    pub fn new(api: ApiClient, endpoint: EndpointBinding) -> Result<Self, ExistsError> {
        if api.selection().audience() != Audience::OneLake
            || endpoint.origin() != ServiceOrigin::OneLake
            || endpoint.audience() != Audience::OneLake
        {
            return Err(ExistsError::ContextMismatch);
        }
        Ok(Self { api, endpoint })
    }

    pub(super) fn matches(&self, host: &ExistsHost<'_>) -> bool {
        self.api
            .selection()
            .tenant()
            .eq_ignore_ascii_case(host.client.selection().tenant())
            && self.api.selection().account() == host.client.selection().account()
    }

    async fn send(
        &self,
        evidence: &Evidence<'_>,
        request: &Request,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Response, ProbeError> {
        cancellable(
            async {
                evidence.before().map_err(|()| ApiError::Limit)?;
                let response = self.api.send(request, limit, pending()).await?;
                evidence
                    .capture(
                        request.url().as_str(),
                        response.status,
                        response
                            .headers
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect(),
                        response.body.clone(),
                    )
                    .map_err(|()| ApiError::Limit)?;
                Ok(response)
            },
            cancelled,
        )
        .await
        .and_then(|result| result)
        .map_err(ProbeError::Catalog)
    }

    pub(super) async fn probe(
        &self,
        evidence: &Evidence<'_>,
        target: &ExistsTarget,
        mut parent: Conclusion,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Conclusion, ProbeError> {
        let (namespace, table) = match target {
            ExistsTarget::CatalogNamespace { namespace, .. } => (namespace, None),
            ExistsTarget::CatalogTable {
                namespace, table, ..
            } => (namespace, Some(table)),
            _ => unreachable!("catalog-only dispatch"),
        };
        let warehouse = format!(
            "{}/{}",
            parent
                .workspace_id
                .as_deref()
                .expect("established workspace"),
            parent.item_id.as_deref().expect("established Lakehouse")
        );
        let config = Request::raw(RawRequestOptions {
            origin: Origin::OneLake,
            method: Method::Get,
            endpoint: "iceberg/v1/config".into(),
            query: vec![("warehouse".into(), warehouse)],
            headers: Vec::new(),
            body: Vec::new(),
            bounds: RequestBounds::default(),
        })
        .and_then(|request| request.with_endpoint(&self.endpoint))
        .map_err(ProbeError::Catalog)?;
        let mut cancelled = pin!(cancelled);
        let response = self
            .send(evidence, &config, limit, cancelled.as_mut())
            .await?;
        if response.status != 200 {
            (parent.state, parent.reason) = status(response.status, false);
            return Ok(parent);
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| ProbeError::Catalog(ApiError::InvalidJson))?;
        let defaults = value
            .get("defaults")
            .and_then(Value::as_object)
            .ok_or(ProbeError::Catalog(ApiError::InvalidResponse))?;
        let overrides = value
            .get("overrides")
            .and_then(Value::as_object)
            .ok_or(ProbeError::Catalog(ApiError::InvalidResponse))?;
        if defaults
            .values()
            .chain(overrides.values())
            .any(|value| !value.is_string())
        {
            return Err(ProbeError::Catalog(ApiError::InvalidResponse));
        }
        let prefix = overrides
            .get("prefix")
            .or_else(|| defaults.get("prefix"))
            .and_then(Value::as_str)
            .filter(|prefix| safe_prefix(prefix))
            .ok_or(ProbeError::Catalog(ApiError::InvalidResponse))?;
        let advertised = match value.get("endpoints") {
            None => {
                parent.state = ExistsObservation::Unknown;
                parent.reason = "CatalogHeadUnavailable";
                return Ok(parent);
            }
            Some(value) => value
                .as_array()
                .filter(|values| values.iter().all(Value::is_string))
                .ok_or(ProbeError::Catalog(ApiError::InvalidResponse))?,
        };
        let required = if table.is_some() {
            "HEAD /v1/{prefix}/namespaces/{namespace}/tables/{table}"
        } else {
            "HEAD /v1/{prefix}/namespaces/{namespace}"
        };
        if !advertised
            .iter()
            .any(|value| value.as_str() == Some(required))
        {
            parent.state = ExistsObservation::Unknown;
            parent.reason = "CatalogHeadUnavailable";
            return Ok(parent);
        }
        let mut url = self
            .endpoint
            .url("/iceberg/v1/")
            .map_err(|_| ProbeError::Catalog(ApiError::InvalidInput))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| ProbeError::Catalog(ApiError::InvalidInput))?;
            segments
                .pop_if_empty()
                .extend(prefix.split('/'))
                .extend(["namespaces", namespace]);
            if let Some(table) = table {
                segments.extend(["tables", table]);
            }
        }
        let head = Request::head(Origin::OneLake, url.path())
            .and_then(|request| request.with_endpoint(&self.endpoint))
            .map_err(ProbeError::Catalog)?;
        let response = self
            .send(evidence, &head, limit, cancelled.as_mut())
            .await?;
        // A nonempty HEAD body is not the documented no-content contract. In
        // particular, never turn a disabled-conversion/policy error into absence.
        if matches!(response.status, 204 | 404) && !response.body.is_empty() {
            parent.state = ExistsObservation::Unknown;
            parent.reason = "UnexpectedCatalogHeadBody";
        } else {
            (parent.state, parent.reason) = status(response.status, true);
        }
        Ok(parent)
    }
}

fn safe_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.len() <= RequestBounds::default().max_url_bytes.get()
        && prefix.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte))
        })
}

fn status(status: u16, head: bool) -> (ExistsObservation, &'static str) {
    use ExistsObservation::{Absent, Denied, Failed, Present, Unknown};
    match (status, head) {
        (204, true) => (Present, "CatalogPresent"),
        (404, true) => (Absent, "CatalogNotFound"),
        (401, _) => (Failed, "AuthenticationRequired"),
        (403, _) => (Denied, "AccessDenied"),
        (404, false) => (Unknown, "CatalogConfigurationUnavailable"),
        (405 | 501, _) => (Unknown, "CatalogBackendUnsupported"),
        (200..=299, _) => (Failed, "UnsupportedStatus"),
        _ => (Unknown, "CatalogServiceError"),
    }
}

/// Additive catalog composition preserving existing metadata-host struct literals.
pub struct CatalogExistsHost<'a> {
    pub(super) metadata: ExistsHost<'a>,
    pub(super) catalog: &'a CatalogBackend,
}

impl CatalogExistsHost<'_> {
    /// Execute the same typed operation with caller-owned cancellation.
    pub async fn execute(
        &self,
        approved: &ApprovedOperation<'_>,
        request: &ExistsRequest,
        limits: ExistsLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ExistsReport, ExistsError> {
        self.execute_until(approved, request, limits, async {
            cancelled.await;
            ExistsInterruption::Cancelled
        })
        .await
    }

    /// Use one caller stopping future across metadata, config and HEAD.
    pub async fn execute_until(
        &self,
        approved: &ApprovedOperation<'_>,
        request: &ExistsRequest,
        limits: ExistsLimits,
        interrupted: impl Future<Output = ExistsInterruption> + Send,
    ) -> Result<ExistsReport, ExistsError> {
        self.metadata
            .execute_inner(approved, request, limits, interrupted, Some(self.catalog))
            .await
    }
}

pub(super) fn parent_request(request: &ExistsRequest) -> Result<ExistsRequest, ExistsError> {
    let (workspace, lakehouse) = match request.target() {
        ExistsTarget::CatalogNamespace {
            workspace,
            lakehouse,
            ..
        }
        | ExistsTarget::CatalogTable {
            workspace,
            lakehouse,
            ..
        } => (workspace, lakehouse),
        _ => return Err(ExistsError::InvalidTarget),
    };
    ExistsRequest::new(
        request.tenant_id(),
        ExistsTarget::Item {
            workspace: workspace.clone(),
            item: lakehouse.clone(),
            item_type: Some("Lakehouse".into()),
        },
    )
}

pub(super) fn unavailable() -> Conclusion {
    probe::conclusion(ExistsObservation::Unknown, "CatalogBackendUnavailable")
}
