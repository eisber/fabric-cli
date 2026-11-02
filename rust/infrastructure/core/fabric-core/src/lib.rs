// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Credential interoperability, explicit endpoints, and request authentication.
//!
//! This crate does not select an identity backend, acquire ambient credentials,
//! initialize a runtime, or configure an HTTP transport.

pub use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
pub use endpoint::{EndpointBinding, EndpointError, ServiceOrigin};
pub use request::{
    AuthenticatedExecution, AuthenticatedFuture, Cancelled, RequestAuthentication, RequestContext,
    RequestError, cancel_with,
};
use thiserror::Error;

/// Explicit service origins and validated endpoint bindings.
pub mod endpoint;
/// Runtime-neutral authentication and cancellation for injected execution.
pub mod request;

/// Public-cloud token audiences. Sovereign-cloud configuration is not yet supported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Audience {
    /// The Fabric REST API.
    Fabric,
    /// OneLake storage.
    OneLake,
    /// Azure Resource Manager.
    Arm,
}

impl Audience {
    /// Returns the explicit public-cloud scope requested from an injected credential.
    pub const fn scope(self) -> &'static str {
        match self {
            Self::Fabric => "https://api.fabric.microsoft.com/.default",
            Self::OneLake => "https://storage.azure.com/.default",
            Self::Arm => "https://management.azure.com/.default",
        }
    }
}

/// Explicit identity selection supplied by the caller, not discovered from the environment.
///
/// The injected provider must be bound to this account and tenant. This metadata
/// alone cannot verify a provider's configuration or the claims of its tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialSelection {
    account: String,
    tenant: String,
    audience: Audience,
}

impl CredentialSelection {
    /// Creates an explicit selection without choosing an identity implementation.
    ///
    /// Account is an opaque, non-secret selector interpreted by the provider.
    /// Neither argument may contain a credential.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::NotConfigured`] for an empty or whitespace-only selector.
    pub fn new(
        account: impl Into<String>,
        tenant: impl Into<String>,
        audience: Audience,
    ) -> Result<Self, IdentityError> {
        let account = account.into();
        let tenant = tenant.into();
        if account.trim().is_empty() || tenant.trim().is_empty() {
            return Err(IdentityError::NotConfigured);
        }
        Ok(Self {
            account,
            tenant,
            audience,
        })
    }

    /// Returns the caller-selected non-secret account selector.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Returns the caller-selected tenant.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Returns the caller-selected audience.
    pub const fn audience(&self) -> Audience {
        self.audience
    }
}

/// Identity failures that must never trigger an implicit fallback.
///
/// Messages deliberately contain no provider response, token, or secret.
/// A future adapter must classify provider failures explicitly rather than
/// treating every acquisition failure as a refresh failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum IdentityError {
    /// The requested backend or capability is not supported.
    #[error("the requested identity capability is unsupported")]
    Unsupported,
    /// Explicit identity configuration is missing.
    #[error("explicit identity configuration is required")]
    NotConfigured,
    /// User interaction is required by the identity provider.
    #[error("identity acquisition requires user interaction")]
    InteractionRequired,
    /// The caller cancelled identity acquisition.
    #[error("identity acquisition was cancelled")]
    Cancelled,
    /// Refresh failed for the explicitly selected identity.
    #[error("the selected identity could not refresh its credential")]
    RefreshFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_selection_is_a_typed_error() {
        for (account, tenant) in [("", "tenant"), ("account", ""), (" ", "\t")] {
            assert_eq!(
                CredentialSelection::new(account, tenant, Audience::Fabric),
                Err(IdentityError::NotConfigured)
            );
        }
    }

    #[test]
    fn selection_preserves_explicit_account_tenant_and_audience() {
        let selection =
            CredentialSelection::new("test-account", "test-tenant", Audience::OneLake).unwrap();
        assert_eq!(selection.account(), "test-account");
        assert_eq!(selection.tenant(), "test-tenant");
        assert_eq!(
            selection.audience().scope(),
            "https://storage.azure.com/.default"
        );
        assert_eq!(
            Audience::Fabric.scope(),
            "https://api.fabric.microsoft.com/.default"
        );
        assert_eq!(
            Audience::Arm.scope(),
            "https://management.azure.com/.default"
        );
    }

    #[test]
    fn identity_failures_remain_distinct_and_sanitized() {
        let cases = [
            (
                IdentityError::Unsupported,
                "the requested identity capability is unsupported",
            ),
            (
                IdentityError::NotConfigured,
                "explicit identity configuration is required",
            ),
            (
                IdentityError::InteractionRequired,
                "identity acquisition requires user interaction",
            ),
            (
                IdentityError::Cancelled,
                "identity acquisition was cancelled",
            ),
            (
                IdentityError::RefreshFailed,
                "the selected identity could not refresh its credential",
            ),
        ];
        for (error, expected) in cases {
            let shared_error: &dyn std::error::Error = &error;
            assert_eq!(shared_error.to_string(), expected);
        }
    }
}
