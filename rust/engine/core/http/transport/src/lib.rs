// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicitly selected native HTTP/TLS and asynchronous caller-runtime time.
//!
//! With no features this crate supplies only [`TokioTimer`]. Enable `hyper-rustls` to
//! construct a reusable `NativeHttpClient` with explicit proxy, trust and
//! socket settings. The SDK facade does not depend on this crate. No runtime,
//! logger, credentials or service requests are created implicitly.

mod timer;
pub use timer::TokioTimer;

#[cfg(feature = "hyper-rustls")]
mod config;
#[cfg(feature = "hyper-rustls")]
mod connector;
#[cfg(feature = "hyper-rustls")]
mod decode;
#[cfg(feature = "hyper-rustls")]
mod error;
#[cfg(feature = "hyper-rustls")]
mod native;

#[cfg(feature = "hyper-rustls")]
pub use config::{ProxyPolicy, RootCertificate, StaticHttpProxy, TransportOptions, TrustPolicy};
#[cfg(feature = "hyper-rustls")]
pub use error::TransportError;
#[cfg(feature = "hyper-rustls")]
pub use native::NativeHttpClient;

#[cfg(all(test, feature = "hyper-rustls"))]
mod test_server;
#[cfg(all(test, feature = "hyper-rustls"))]
mod tests;
