// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    BrokerError, EntraOptions, Interaction, NativeOperation, NativeStage, TokenAudience,
    entra_contract::{needs_interaction, request_status, response_identity, unix_expiry},
    native::{ParentWindow, native_error, spawn, start_interactive, wait},
    request::Shared,
};
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};
use windows::{
    Security::{
        Authentication::Web::Core::{
            FindAllWebAccountsStatus, WebAuthenticationCoreManager as Manager, WebTokenRequest,
            WebTokenRequestPromptType, WebTokenRequestResult, WebTokenRequestStatus,
        },
        Credentials::{WebAccount, WebAccountProvider},
    },
    core::HSTRING,
};

/// A broker-issued token with authoritative expiry; never formatted with its value.
pub struct EntraToken {
    audience: TokenAudience,
    expires_on: i64,
    token: String,
}

impl EntraToken {
    /// The separately requested public-cloud audience.
    pub const fn audience(&self) -> TokenAudience {
        self.audience
    }
    /// Broker-supplied expiry, converted to Unix seconds without parsing the token.
    pub const fn expires_on(&self) -> i64 {
        self.expires_on
    }
    /// Consumes the token for an explicitly configured credential adapter.
    ///
    /// The returned value is a credential. Never print it or persist it as plaintext.
    pub fn into_secret(self) -> String {
        self.token
    }
}

impl fmt::Debug for EntraToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntraToken")
            .field("audience", &self.audience)
            .field("expires_on", &self.expires_on)
            .finish_non_exhaustive()
    }
}

/// Validated selected-account acquisition; partial results are never published.
pub struct EntraSession {
    account_id: String,
    tokens: Vec<EntraToken>,
}

impl EntraSession {
    /// Stable native account identifier, not a credential or inferred tenant.
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    /// Consume the per-resource tokens for an explicitly bound caller.
    pub fn into_tokens(self) -> Vec<EntraToken> {
        self.tokens
    }
}

impl fmt::Debug for EntraSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntraSession")
            .field("tokens", &self.tokens)
            .finish_non_exhaustive()
    }
}

/// Acquires the selected account's tokens, silently first and optionally through MFA UI.
///
/// `account_id` pins a previously established native identity. `None` still requires
/// an explicit UPN and tenant; no first-account or OS-default fallback is allowed.
/// Results are published only after every requested audience succeeds. WAM owns
/// its cache; partial acquisition can update that cache even if this call fails.
///
/// This function may contact Entra and display UI only when `interaction` permits.
/// It does not call Fabric/Storage/ARM or write a local credential file.
pub fn acquire(
    options: EntraOptions,
    account_id: Option<String>,
    audiences: Vec<TokenAudience>,
    interaction: Interaction,
) -> NativeOperation<EntraSession> {
    spawn(move |shared| {
        if audiences.is_empty() {
            return Err(BrokerError::InvalidConfiguration);
        }
        let provider = provider(&options, shared)?;
        let mut account = find_account(&provider, &options, account_id.as_deref(), shared)?;
        if account.is_none() && interaction == Interaction::Silent {
            return Err(BrokerError::InteractionRequired);
        }
        let mut binding = account_id;
        let mut tokens = Vec::new();
        for audience in audiences {
            cancelled(shared)?;
            let result = if let Some(selected) = &account {
                let request = request(&provider, &options, audience, false)?;
                let operation = Manager::GetTokenSilentlyWithWebAccountAsync(&request, selected)
                    .map_err(|error| native_error(NativeStage::Request, error))?;
                let result = wait(operation, shared)?;
                let status = result
                    .ResponseStatus()
                    .map_err(|error| native_error(NativeStage::Response, error))?;
                if needs_interaction(status.0, interaction) {
                    interactive(&provider, &options, audience, account.clone(), shared)?
                } else {
                    result
                }
            } else {
                interactive(&provider, &options, audience, None, shared)?
            };
            classify(&result)?;
            let responses = result
                .ResponseData()
                .map_err(|error| native_error(NativeStage::Response, error))?;
            if responses
                .Size()
                .map_err(|error| native_error(NativeStage::Response, error))?
                != 1
            {
                return Err(BrokerError::InvalidResponse);
            }
            let response = responses
                .GetAt(0)
                .map_err(|error| native_error(NativeStage::Response, error))?;
            let selected = response
                .WebAccount()
                .map_err(|_| BrokerError::InvalidResponse)?;
            let id = selected
                .Id()
                .map_err(|_| BrokerError::InvalidResponse)?
                .to_string();
            let username = selected
                .UserName()
                .map_err(|_| BrokerError::InvalidResponse)?
                .to_string();
            let properties = response
                .Properties()
                .map_err(|_| BrokerError::InvalidResponse)?;
            let authority = properties
                .Lookup(&HSTRING::from("Authority"))
                .map_err(|_| BrokerError::InvalidResponse)?
                .to_string();
            response_identity(&options, binding.as_deref(), &id, &username, &authority)?;
            let expires = properties
                .Lookup(&HSTRING::from("TokenExpiresOn"))
                .map_err(|_| BrokerError::InvalidResponse)?
                .to_string();
            let expires_on = unix_expiry(&expires, unix_now()?)?;
            let token = response
                .Token()
                .map_err(|_| BrokerError::InvalidResponse)?
                .to_string();
            if token.is_empty() {
                return Err(BrokerError::InvalidResponse);
            }
            binding = Some(id);
            account = Some(selected);
            tokens.push(EntraToken {
                audience,
                expires_on,
                token,
            });
        }
        cancelled(shared)?;
        let now = unix_now()?;
        if tokens.iter().any(|token| token.expires_on <= now) {
            return Err(BrokerError::InvalidResponse);
        }
        Ok(EntraSession {
            account_id: binding.ok_or(BrokerError::InvalidResponse)?,
            tokens,
        })
    })
}

fn unix_now() -> Result<i64, BrokerError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or(BrokerError::InvalidResponse)
}

/// Signs out only the explicitly selected account for this client on this device.
///
/// Never removes a Windows account. Missing accounts succeed without trying
/// another identity. This is a live broker/cache operation, not server revocation.
pub fn logout(options: EntraOptions) -> NativeOperation<()> {
    spawn(move |shared| {
        let provider = provider(&options, shared)?;
        if let Some(account) = find_account(&provider, &options, None, shared)? {
            cancelled(shared)?;
            let operation = account
                .SignOutWithClientIdAsync(&HSTRING::from(options.client()))
                .map_err(|error| native_error(NativeStage::Logout, error))?;
            wait(operation, shared)?;
        }
        Ok(())
    })
}

fn cancelled<T>(shared: &Shared<T>) -> Result<(), BrokerError> {
    if shared.is_cancelled() {
        Err(BrokerError::Cancelled)
    } else {
        Ok(())
    }
}

fn provider<T>(
    options: &EntraOptions,
    shared: &Shared<T>,
) -> Result<WebAccountProvider, BrokerError> {
    cancelled(shared)?;
    let operation = Manager::FindAccountProviderWithAuthorityAsync(
        &HSTRING::from("https://login.microsoft.com"),
        &HSTRING::from(options.authority()),
    )
    .map_err(|error| native_error(NativeStage::Provider, error))?;
    wait(operation, shared).map_err(|error| match error {
        BrokerError::Native {
            code: -2147467261, ..
        } => BrokerError::ProviderUnavailable,
        other => other,
    })
}

fn find_account<T>(
    provider: &WebAccountProvider,
    options: &EntraOptions,
    binding: Option<&str>,
    shared: &Shared<T>,
) -> Result<Option<WebAccount>, BrokerError> {
    cancelled(shared)?;
    let operation =
        Manager::FindAllAccountsWithClientIdAsync(provider, &HSTRING::from(options.client()))
            .map_err(|error| native_error(NativeStage::Account, error))?;
    let result = wait(operation, shared)?;
    let status = result
        .Status()
        .map_err(|error| native_error(NativeStage::Account, error))?;
    if status != FindAllWebAccountsStatus::Success {
        if status != FindAllWebAccountsStatus::ProviderError {
            return Err(BrokerError::AccountsUnavailable { status: status.0 });
        }
        let code = result
            .ProviderError()
            .and_then(|error| error.ErrorCode())
            .map_err(|error| native_error(NativeStage::Account, error))?;
        return Err(BrokerError::Provider {
            status: status.0,
            code,
        });
    }
    let accounts = result
        .Accounts()
        .map_err(|error| native_error(NativeStage::Account, error))?;
    let mut matched = None;
    for index in 0..accounts
        .Size()
        .map_err(|error| native_error(NativeStage::Account, error))?
    {
        let account = accounts
            .GetAt(index)
            .map_err(|error| native_error(NativeStage::Account, error))?;
        let username = account
            .UserName()
            .map_err(|error| native_error(NativeStage::Account, error))?;
        let id = account
            .Id()
            .map_err(|error| native_error(NativeStage::Account, error))?;
        if username.to_string().eq_ignore_ascii_case(options.account())
            && binding.is_none_or(|expected| expected == id)
        {
            if matched.is_some() {
                return Err(BrokerError::AmbiguousAccount);
            }
            matched = Some(account);
        }
    }
    if binding.is_some() && matched.is_none() {
        return Err(BrokerError::IdentityMismatch);
    }
    Ok(matched)
}

fn request(
    provider: &WebAccountProvider,
    options: &EntraOptions,
    audience: TokenAudience,
    interactive: bool,
) -> Result<WebTokenRequest, BrokerError> {
    let scope = format!("{} openid profile offline_access", audience.scope());
    let prompt = if interactive {
        WebTokenRequestPromptType::ForceAuthentication
    } else {
        WebTokenRequestPromptType::Default
    };
    let request = WebTokenRequest::CreateWithPromptType(
        provider,
        &HSTRING::from(scope),
        &HSTRING::from(options.client()),
        prompt,
    )
    .map_err(|error| native_error(NativeStage::Request, error))?;
    let properties = request
        .Properties()
        .map_err(|error| native_error(NativeStage::Request, error))?;
    for (key, value) in options.request_properties(interactive) {
        properties
            .Insert(&HSTRING::from(key), &HSTRING::from(value))
            .map_err(|error| native_error(NativeStage::Request, error))?;
    }
    Ok(request)
}

fn interactive<T>(
    provider: &WebAccountProvider,
    options: &EntraOptions,
    audience: TokenAudience,
    account: Option<WebAccount>,
    shared: &Shared<T>,
) -> Result<WebTokenRequestResult, BrokerError> {
    cancelled(shared)?;
    let request = request(provider, options, audience, true)?;
    let mut parent = ParentWindow::new()?;
    let result = cancelled(shared)
        .and_then(|()| start_interactive(request, account, &parent))
        .and_then(|operation| wait(operation, shared));
    parent.close()?;
    result
}

fn classify(result: &WebTokenRequestResult) -> Result<(), BrokerError> {
    let status = result
        .ResponseStatus()
        .map_err(|error| native_error(NativeStage::Response, error))?;
    let code = if status == WebTokenRequestStatus::ProviderError {
        result
            .ResponseError()
            .and_then(|error| error.ErrorCode())
            .map_err(|error| native_error(NativeStage::Response, error))?
    } else {
        0
    };
    request_status(status.0, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_and_native_account_ids_are_not_in_debug_output() {
        let session = EntraSession {
            account_id: "private-native-account-id".into(),
            tokens: vec![EntraToken {
                audience: TokenAudience::Fabric,
                expires_on: 1_700_003_600,
                token: "distinctive-test-only-secret".into(),
            }],
        };
        let debug = format!("{session:?}");
        assert!(!debug.contains("distinctive-test-only-secret"));
        assert!(!debug.contains("private-native-account-id"));
    }
}
