// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::{Error, ErrorKind},
    time::OffsetDateTime,
};
use fabric_identity_windows::{self as wam, BrokerError, EntraOptions, Interaction, TokenAudience};
use std::{fmt, sync::Arc};

/// Explicit public-cloud account/tenant/client configuration.
pub use wam::EntraOptions as LoginOptions;

/// Non-secret token metadata safe to report after successful acquisition.
#[derive(Clone, Copy, Debug)]
pub struct TokenMetadata {
    /// Exact requested scope.
    pub scope: &'static str,
    /// Broker-derived expiry in Unix seconds.
    pub expires_on: i64,
}

/// An account/tenant-bound WAM credential that refreshes silently for SDK calls.
///
/// Only `login` can authorize interactive UI. `get_token` never prompts, switches
/// accounts or uses an alternate backend. The broker, not this object, owns cache
/// persistence. No token is stored in this object's Debug or local configuration.
pub struct WindowsCredential {
    options: EntraOptions,
    account_id: String,
    metadata: Vec<TokenMetadata>,
}

impl WindowsCredential {
    /// Acquires Fabric, OneLake and ARM tokens in Python login order.
    ///
    /// This is a live operation when explicitly invoked; allow interaction only
    /// from a user-initiated CLI action. All audiences must succeed before returning.
    /// It may update the broker cache even when cancelled or partially unsuccessful.
    ///
    /// # Errors
    ///
    /// Returns sanitized native/configuration/interaction errors; never a token.
    pub async fn login(
        options: EntraOptions,
        allow_interaction: bool,
    ) -> Result<Arc<Self>, BrokerError> {
        let interaction = if allow_interaction {
            Interaction::Allowed
        } else {
            Interaction::Silent
        };
        let session = wam::acquire(
            options.clone(),
            None,
            vec![
                TokenAudience::Fabric,
                TokenAudience::OneLake,
                TokenAudience::Arm,
            ],
            interaction,
        )
        .await?;
        let account_id = session.account_id().to_owned();
        let metadata = session
            .into_tokens()
            .into_iter()
            .map(|token| TokenMetadata {
                scope: token.audience().scope(),
                expires_on: token.expires_on(),
            })
            .collect();
        Ok(Arc::new(Self {
            options,
            account_id,
            metadata,
        }))
    }

    /// Metadata from the successful login, not a new token or current status check.
    pub fn metadata(&self) -> &[TokenMetadata] {
        &self.metadata
    }
}

impl fmt::Debug for WindowsCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsCredential").finish_non_exhaustive()
    }
}

#[async_trait]
impl TokenCredential for WindowsCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let audience = match scopes {
            ["https://api.fabric.microsoft.com/.default"] => TokenAudience::Fabric,
            ["https://storage.azure.com/.default"] => TokenAudience::OneLake,
            ["https://management.azure.com/.default"] => TokenAudience::Arm,
            _ => {
                return Err(Error::with_message(
                    ErrorKind::Credential,
                    "unsupported credential scope",
                ));
            }
        };
        let session = wam::acquire(
            self.options.clone(),
            Some(self.account_id.clone()),
            vec![audience],
            Interaction::Silent,
        )
        .await
        .map_err(|error| Error::new(ErrorKind::Credential, error))?;
        let token = session.into_tokens().pop().ok_or_else(|| {
            Error::with_message(ErrorKind::Credential, "the broker returned no token")
        })?;
        let expires = OffsetDateTime::from_unix_timestamp(token.expires_on())
            .map_err(|_| Error::with_message(ErrorKind::Credential, "invalid broker expiry"))?;
        Ok(AccessToken::new(token.into_secret(), expires))
    }
}

/// Signs out the selected application's broker session on this device.
///
/// Does not remove the Windows account or revoke sessions on other devices.
/// Calling this is an explicit live operation; no local plaintext cache is used.
///
/// # Errors
///
/// Returns sanitized broker errors; does not report success on a failed sign-out.
pub async fn sign_out(options: EntraOptions) -> Result<(), BrokerError> {
    wam::logout(options).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_rejects_unknown_or_combined_scopes_before_native_access() {
        let credential = WindowsCredential {
            options: EntraOptions::new(
                "11111111-2222-3333-4444-555555555555",
                "11111111-2222-3333-4444-555555555555",
                "user@example.test",
            )
            .unwrap(),
            account_id: "not-a-real-native-account".into(),
            metadata: Vec::new(),
        };
        for scopes in [
            vec![],
            vec!["https://evil.example/.default"],
            vec![
                TokenAudience::Fabric.scope(),
                TokenAudience::OneLake.scope(),
            ],
        ] {
            let error = credential.get_token(&scopes, None).await.unwrap_err();
            assert_eq!(error.to_string(), "unsupported credential scope");
        }
        assert_eq!(format!("{credential:?}"), "WindowsCredential { .. }");
    }
}
