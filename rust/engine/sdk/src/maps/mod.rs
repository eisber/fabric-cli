// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Experimental Azure Maps processing contracts, not public Fabric v1 APIs.
//!
//! These pure builders never discover credentials or contact services. Execution
//! requires an explicitly admitted private Shared issuer and caller-owned HTTP.

mod request;
mod result;

pub use request::{Address, Coordinate, Operation, PreparedRequest, RouteOptions, Units};
pub use result::{MatrixCell, ProcessingResult};

/// Maximum synchronous matrix size accepted by this conservative client.
pub const MAX_MATRIX_CELLS: usize = 100;
/// Maximum serialized provider request size.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024;
/// Maximum decoded provider response size.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Maximum encoded or expanded result-data output size.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Explicit private issuer audience; never the Azure Maps provider audience.
pub const POWER_BI_SCOPE: &str = "https://analysis.windows.net/powerbi/api/.default";
/// The only supported processing origin.
pub const PROVIDER_ORIGIN: &str = "https://atlas.microsoft.com";

/// Sanitized contract failure; no provider body, query or credential is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapsError {
    /// Invalid or excessive input, rejected before credentials or networking.
    InvalidInput,
    /// A returned discovery origin is outside the trusted HTTPS boundary.
    UntrustedOrigin,
    /// A response is malformed, incomplete, duplicate or out of bounds.
    InvalidResponse,
}

impl std::fmt::Display for MapsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "Invalid or excessive Maps input.",
            Self::UntrustedOrigin => "Shared discovery returned an untrusted HTTPS origin.",
            Self::InvalidResponse => "Maps returned a malformed or incomplete response.",
        })
    }
}

impl std::error::Error for MapsError {}

/// Validate a discovered Shared root before forwarding any Entra credential.
///
/// Only public-cloud Microsoft Analysis subdomains, without user information,
/// query, fragment, nondefault port or service path, are accepted.
pub fn shared_issuer(backend: &str) -> Result<azure_core::http::Url, MapsError> {
    use azure_core::http::Url;
    let url = Url::parse(backend).map_err(|_| MapsError::UntrustedOrigin)?;
    let host = url.host_str().ok_or(MapsError::UntrustedOrigin)?;
    if url.scheme() != "https"
        || !host.ends_with(".analysis.windows.net")
        || host.len() <= ".analysis.windows.net".len()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MapsError::UntrustedOrigin);
    }
    url.join("metadata/v202212/azureMapToken")
        .map_err(|_| MapsError::UntrustedOrigin)
}
