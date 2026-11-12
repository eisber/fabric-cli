// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared safe response-head evidence, independent of HTTP/LRO implementations.
use crate::{
    ApiResponse, Audience, CoreError, ErrorKind, RecoveryInfo, validate_path,
    validate_response_headers, validate_url,
};
use std::{collections::BTreeMap, sync::Mutex};
use url::Url;

/// Recognized operation protocol advertised by an accepted response head.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationKind {
    /// Fabric status JSON.
    Fabric,
    /// ARM Azure-AsyncOperation status JSON.
    ArmStatus,
    /// ARM Location HTTP status semantics.
    ArmLocation,
}
/// Validated same-origin operation handle. No bearer/custom request headers.
#[derive(Clone)]
pub struct OperationHandle {
    /// Full validated status URL, including any query needed for polling.
    url: Url,
    /// Validated Fabric operation ID when known.
    operation_id: Option<String>,
    /// Advertised protocol.
    kind: OperationKind,
}
impl std::fmt::Debug for OperationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationHandle")
            .field("recovery", &self.recovery())
            .field("kind", &self.kind)
            .finish()
    }
}
impl OperationHandle {
    /// Full validated status URL, including any query needed for polling.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Validated operation identifier, when available.
    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }
    /// Recognized protocol.
    pub fn kind(&self) -> OperationKind {
        self.kind
    }
    /// Safe recovery evidence, excluding query values.
    pub fn recovery(&self) -> RecoveryInfo {
        let mut url = self.url.clone();
        url.set_query(None);
        RecoveryInfo {
            operation_id: self.operation_id.clone(),
            status_url: url.to_string(),
            outcome_uncertain: true,
        }
    }
    /// Preserve this known handle on a safe error.
    pub fn attach(&self, mut error: CoreError) -> CoreError {
        error.recovery = Some(self.recovery());
        error
    }
}
fn protocol(message: &str) -> CoreError {
    CoreError::new(ErrorKind::Protocol, message)
}

/// Validate a status/result URL before normalization can hide unsafe paths.
pub fn validate_operation_url(audience: Audience, value: &str) -> Result<Url, CoreError> {
    if value.len() > 16 * 1024 || value.chars().any(|c| c.is_control() || c == ' ') {
        return Err(protocol("Unsafe operation URL"));
    }
    validate_path(value.split(['?', '#']).next().unwrap_or_default())
        .map_err(|_| protocol("Unsafe operation URL"))?;
    let url = if value.starts_with("https://") {
        Url::parse(value)
    } else if value.starts_with('/') && !value.starts_with("//") {
        Url::parse(audience.base_url()).and_then(|base| base.join(value))
    } else {
        return Err(protocol(
            "Operation URL must be absolute HTTPS or origin-relative",
        ));
    }
    .map_err(|_| protocol("Malformed operation URL"))?;
    validate_url(audience, &url).map_err(|_| protocol("Unsafe operation URL"))?;
    Ok(url)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
fn fabric_id(url: &Url) -> Option<String> {
    let tail = url
        .path()
        .strip_prefix("/v1/operations/")?
        .trim_end_matches('/');
    valid_id(tail).then(|| tail.to_owned())
}
fn id_handle(id: &str) -> OperationHandle {
    OperationHandle {
        url: Url::parse(&format!("{}/operations/{id}", Audience::Fabric.base_url()))
            .expect("fixed URL and safe ID"),
        operation_id: Some(id.to_owned()),
        kind: OperationKind::Fabric,
    }
}
// A rejected head is not safe to interpret as an operation advertisement.
// Independently retain only a single valid ID: never join/select duplicate IDs,
// salvage a Location, or turn the header error into permission to poll.
fn independent_id_recovery<'a>(
    audience: Audience,
    status: u16,
    headers: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Option<RecoveryInfo> {
    if audience != Audience::Fabric || status != 202 {
        return None;
    }
    let mut seen = false;
    let mut candidate = None;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("x-ms-operation-id") {
            if seen {
                return None;
            }
            seen = true;
            candidate = std::str::from_utf8(value).ok().filter(|id| valid_id(id));
        }
    }
    candidate.map(|id| id_handle(id).recovery())
}
fn discover(audience: Audience, head: &ApiResponse) -> Result<Option<OperationHandle>, CoreError> {
    match audience {
        Audience::Fabric if head.status == 202 => {
            let id = head.header("x-ms-operation-id");
            if id.is_some_and(|id| !valid_id(id)) {
                return Err(protocol("Malformed Fabric operation identifier"));
            }
            if let Some(location) = head.header("location") {
                let url = validate_operation_url(audience, location)?;
                if url.path().contains("/jobs/instances/") {
                    return Ok(None);
                }
                let Some(location_id) = fabric_id(&url) else {
                    if id.is_none() {
                        return Ok(None);
                    }
                    return Err(protocol(
                        "Fabric operation ID conflicts with unrecognized Location",
                    ));
                };
                let handle = OperationHandle {
                    url,
                    operation_id: Some(location_id.clone()),
                    kind: OperationKind::Fabric,
                };
                if id.is_some_and(|id| id != location_id) {
                    return Err(handle.attach(protocol("Conflicting Fabric operation handles")));
                }
                return Ok(Some(handle));
            }
            Ok(id.map(id_handle))
        }
        Audience::Azure if matches!(head.status, 201 | 202) => {
            let selected = head
                .header("azure-asyncoperation")
                .map(|v| (v, OperationKind::ArmStatus))
                .or_else(|| {
                    head.header("location")
                        .map(|v| (v, OperationKind::ArmLocation))
                });
            selected
                .map(|(location, kind)| {
                    validate_operation_url(audience, location).map(|url| {
                        Some(OperationHandle {
                            url,
                            operation_id: None,
                            kind,
                        })
                    })
                })
                .unwrap_or(Ok(None))
        }
        _ => Ok(None),
    }
}
/// Interpret only validated bounded response-head fields. Unsafe/conflicting
/// handles fail explicitly and retain separately validated recovery evidence.
/// This shared implementation is used before body reads and by LRO discovery.
pub fn operation_handle(
    audience: Audience,
    status: u16,
    headers: &BTreeMap<String, String>,
) -> Result<Option<OperationHandle>, CoreError> {
    validate_response_headers(headers).map_err(|mut error| {
        error.recovery = independent_id_recovery(
            audience,
            status,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        );
        error
    })?;
    let head = ApiResponse {
        status,
        headers: headers.clone(),
        body: Vec::new(),
    };
    discover(audience, &head).map_err(|mut error| {
        if error.recovery.is_none() && audience == Audience::Fabric && status == 202 {
            if let Some(id) = head.header("x-ms-operation-id").filter(|id| valid_id(id)) {
                error = id_handle(id).attach(error);
            } else if let Some(handle) = head
                .header("location")
                .and_then(|value| validate_operation_url(audience, value).ok())
                .and_then(|url| {
                    fabric_id(&url).map(|id| OperationHandle {
                        url,
                        operation_id: Some(id),
                        kind: OperationKind::Fabric,
                    })
                })
            {
                error = handle.attach(error);
            }
        }
        error
    })
}

/// Caller-owned safe head capture, kept alive outside body/transport futures so
/// dropping those futures on timeout/cancellation does not erase recovery.
/// Only validated recovery data is retained, never arbitrary headers or tokens.
#[derive(Debug)]
pub struct ResponseHeadCapture {
    audience: Option<Audience>,
    evidence: Mutex<HeadEvidence>,
}
#[derive(Debug, Default)]
struct HeadEvidence {
    recovery: Option<RecoveryInfo>,
    problem: Option<String>,
}
impl ResponseHeadCapture {
    /// Capture evidence for a request bound to the selected audience.
    pub fn new(audience: Audience) -> Self {
        Self {
            audience: Some(audience),
            evidence: Mutex::new(HeadEvidence::default()),
        }
    }
    /// Capture recognized service evidence for a direct generic transport call.
    /// Unknown HTTPS origins do not produce service-operation evidence.
    pub fn for_url(url: &Url) -> Self {
        Self {
            audience: [
                Audience::Fabric,
                Audience::Storage,
                Audience::Azure,
                Audience::PowerBi,
            ]
            .into_iter()
            .find(|audience| validate_url(*audience, url).is_ok()),
            evidence: Mutex::new(HeadEvidence::default()),
        }
    }
    /// Record a complete bounded response head before any body read or decode.
    /// Malformed HTTP fields fail here; operation-handle errors are retained as
    /// evidence, not raised yet, so raw/async execution still returns the actual
    /// response. Reconciliation validates handles before making any poll.
    pub fn record(&self, status: u16, headers: &BTreeMap<String, String>) -> Result<(), CoreError> {
        *self
            .evidence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = HeadEvidence::default();
        if let Err(error) = validate_response_headers(headers) {
            return Err(self.reject_headers(
                status,
                headers
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_bytes())),
                error,
            ));
        }
        let result = if let Some(audience) = self.audience {
            operation_handle(audience, status, headers)
        } else {
            Ok(None)
        };
        let evidence = match result {
            Ok(handle) => HeadEvidence {
                recovery: handle.as_ref().map(OperationHandle::recovery),
                problem: None,
            },
            Err(error) => HeadEvidence {
                recovery: error.recovery,
                problem: Some(error.message),
            },
        };
        *self
            .evidence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = evidence;
        Ok(())
    }
    /// Retain independently safe recovery while returning a header-ingestion
    /// error unchanged in kind/message. Supply all raw fields, including every
    /// duplicate; only one valid Fabric ID can provide evidence. Locations are
    /// never salvaged from an invalid head, and this does not authorize polling.
    pub fn reject_headers<'a>(
        &self,
        status: u16,
        headers: impl IntoIterator<Item = (&'a str, &'a [u8])>,
        mut error: CoreError,
    ) -> CoreError {
        let recovery = self
            .audience
            .and_then(|audience| independent_id_recovery(audience, status, headers));
        error.recovery = recovery.clone();
        *self
            .evidence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = HeadEvidence {
            recovery,
            problem: Some(error.message.clone()),
        };
        error
    }
    /// Snapshot only safe recovery data.
    pub fn recovery(&self) -> Option<RecoveryInfo> {
        self.evidence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recovery
            .clone()
    }
    /// Attach captured evidence to body, decode, deadline, or cancellation errors.
    pub fn attach(&self, mut error: CoreError) -> CoreError {
        let evidence = self
            .evidence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(recovery) = &evidence.recovery {
            error.recovery = Some(recovery.clone());
        }
        if let Some(problem) = &evidence.problem
            && !error.message.contains(problem.as_str())
        {
            error.message.push_str("; ");
            error.message.push_str(problem);
        }
        error
    }
}
