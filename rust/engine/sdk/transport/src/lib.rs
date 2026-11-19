// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Compatible transport imports and optional service-specific wire adapters.
//!
//! Generic HTTP/TLS lives in `fabric-http-transport`, which has no service or
//! facade dependency, including when its native transport feature is enabled.

use fabric_http::{ExecutionFuture, ExecutionTimer};
use std::time::Duration;

#[cfg(feature = "hyper-rustls")]
pub use fabric_http_transport::{
    NativeHttpClient, ProxyPolicy, RootCertificate, StaticHttpProxy, TransportError,
    TransportOptions, TrustPolicy,
};

#[cfg(feature = "notebook-websocket")]
pub mod notebook_websocket;

/// Compatibility timer on the caller's Tokio runtime; never creates a runtime.
#[derive(Clone, Copy, Debug)]
pub struct TokioTimer;

impl ExecutionTimer for TokioTimer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        fabric_http_transport::TokioTimer.sleep(delay)
    }
}
