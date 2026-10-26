// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::ExecutionError;
use azure_core::http::{Method, Url};
use fabric_sdk::Audience;

/// Finite public origin and credential-scope pairs for explicit service adapters.
///
/// Selection is supplied by the caller, never inferred from a URL. Existing
/// Fabric-only executors remain Fabric-only. Sovereign/custom hosts are excluded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicOrigin {
    /// Fabric REST with the Fabric audience.
    Fabric,
    /// OneLake DFS with the storage audience.
    OneLake,
    /// Azure Resource Manager with the ARM audience.
    Arm,
    /// Power BI, explicitly preserving Python's Fabric token scope.
    PowerBi,
}

impl PublicOrigin {
    /// Returns the explicit scope selector required from the caller's provider.
    ///
    /// This metadata cannot verify the audience of an already-issued opaque token.
    pub const fn audience(self) -> Audience {
        match self {
            Self::Fabric | Self::PowerBi => Audience::Fabric,
            Self::OneLake => Audience::OneLake,
            Self::Arm => Audience::Arm,
        }
    }

    pub(crate) const fn host(self) -> &'static str {
        match self {
            Self::Fabric => "api.fabric.microsoft.com",
            Self::OneLake => "onelake.dfs.fabric.microsoft.com",
            Self::Arm => "management.azure.com",
            Self::PowerBi => "api.powerbi.com",
        }
    }

    pub(crate) fn validate_url(self, url: &Url) -> Result<(), ExecutionError> {
        if url.scheme() != "https"
            || url.host_str() != Some(self.host())
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(ExecutionError::InvalidInput);
        }
        Ok(())
    }
}

pub(crate) fn validate_request(
    method: Method,
    url: &Url,
    body: &[u8],
    headers: &[(String, String)],
) -> Result<(), ExecutionError> {
    if !matches!(
        method,
        Method::Get | Method::Head | Method::Post | Method::Put | Method::Patch | Method::Delete
    ) || url.as_str().len() > 16 * 1024
        || body.len() > 4 * 1024 * 1024
        || headers.len() > 64
    {
        return Err(ExecutionError::InvalidInput);
    }
    let mut bytes = 0_usize;
    for (name, value) in headers {
        bytes = bytes
            .checked_add(name.len())
            .and_then(|n| n.checked_add(value.len()))
            .and_then(|n| n.checked_add(4))
            .ok_or(ExecutionError::InvalidInput)?;
        if bytes > 16 * 1024
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.chars().any(char::is_control)
            || matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization"
                    | "host"
                    | "proxy-authorization"
                    | "proxy-authenticate"
                    | "cookie"
                    | "set-cookie"
                    | "connection"
                    | "transfer-encoding"
                    | "content-length"
                    | "upgrade"
                    | "te"
                    | "trailer"
            )
        {
            return Err(ExecutionError::InvalidInput);
        }
    }
    Ok(())
}
