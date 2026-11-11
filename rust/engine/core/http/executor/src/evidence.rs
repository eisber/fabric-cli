// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in per-command HTTP evidence without request/response payload capture.

use crate::ExecutionError;
use azure_core::{
    error::ErrorKind,
    http::{Method, headers::Headers},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// One latest exchange; each new request clears status and correlation fields.
#[derive(Clone, Debug, Default)]
pub struct ExecutionEvidence {
    /// Attempted HTTP exchanges, including existing bounded GET retries.
    pub requests: usize,
    /// Registered method only, never URL, token or body.
    pub method: Option<String>,
    /// `dispatch`, `response_body`, or `complete`.
    pub phase: &'static str,
    /// Status received before streaming, retained even on stream failure.
    pub status: Option<u16>,
    /// Allowlisted bounded opaque request/activity/correlation IDs only.
    pub request_ids: BTreeMap<String, String>,
    /// Validated operation UUID observed in headers, including incomplete bodies.
    pub operation_id: Option<String>,
    /// Actual Azure error kind; an I/O kind does not prove a specific timeout.
    pub transport_kind: Option<&'static str>,
    /// Sanitized HTTP executor classification, independently of LRO mapping.
    pub failure: Option<ExecutionError>,
}

/// Caller-owned observation shared only by explicitly selected executor clones.
#[derive(Clone, Debug, Default)]
pub struct ExecutionTrace(Arc<Mutex<ExecutionEvidence>>);

impl ExecutionTrace {
    /// Copy bounded evidence. No model input/output or arbitrary header is stored.
    pub fn snapshot(&self) -> ExecutionEvidence {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn start(&self, method: Method) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requests = state.requests.saturating_add(1);
        *state = ExecutionEvidence {
            requests,
            method: Some(method.as_str().into()),
            phase: "dispatch",
            ..ExecutionEvidence::default()
        };
    }

    pub(crate) fn headers(&self, status: u16, headers: &Headers) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.status = Some(status);
        state.phase = "response_body";
        state.operation_id = headers
            .get_optional_str(&"x-ms-operation-id".into())
            .filter(|value| {
                value.len() == 36
                    && value.bytes().enumerate().all(|(index, byte)| {
                        if matches!(index, 8 | 13 | 18 | 23) {
                            byte == b'-'
                        } else {
                            byte.is_ascii_hexdigit()
                        }
                    })
            })
            .map(str::to_owned);
        for key in [
            "request-id",
            "x-ms-request-id",
            "x-ms-activity-id",
            "activity-id",
            "x-ms-correlation-request-id",
        ] {
            if let Some(value) = headers.get_optional_str(&key.into()).filter(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'_' | b'.' | b'{' | b'}')
                    })
            }) {
                state.request_ids.insert(key.into(), value.into());
            }
        }
    }

    pub(crate) fn transport(&self, kind: &ErrorKind) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transport_kind = Some(match kind {
            ErrorKind::Connection => "connection",
            ErrorKind::Io => "io",
            ErrorKind::DataConversion => "data_conversion",
            ErrorKind::Credential => "credential",
            _ => "other",
        });
    }

    pub(crate) fn finish(&self, failure: Option<ExecutionError>) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failure = failure;
        if failure.is_none() {
            state.phase = "complete";
        }
    }
}
