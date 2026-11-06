// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit authentication over the existing experimental Windows broker.
//!
//! This bounded profile never discovers selectors, invokes the account picker,
//! reads configuration, or persists a v2 selection. WAM owns its secure cache.
//! Successful acquisition is not evidence of live MFA, tenant-policy acceptance,
//! or authorization to any service.

use crate::{CliError, OperationResult};
#[cfg(feature = "windows-broker")]
use crate::{EffectKind, EffectState};

/// Existing public-client identifier; registration and tenant policy still apply.
pub const DEFAULT_CLIENT_ID: &str = "5814bfb4-2705-4994-b8d6-39aabeb5eaeb";

/// The two selected authentication operations; status is not a v2 command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthAction {
    /// Explicitly acquire the selected identity's three required audiences.
    Login,
    /// Invalidate this process's credentials and request app/device sign-out.
    Logout,
}

/// Non-secret selectors supplied explicitly by the caller, never discovered.
#[derive(Clone)]
pub struct AuthRequest {
    /// Explicit work-account UPN, not a password or access token.
    pub account: String,
    /// Explicit public-cloud target tenant GUID.
    pub tenant: String,
    /// Public-client registration GUID.
    pub client_id: String,
    /// Whether login may display broker UI; logout never honors this permission.
    pub allow_interaction: bool,
    /// Total outer CLI wait, in seconds, from 1 through 600.
    pub timeout_seconds: u64,
}

impl std::fmt::Debug for AuthRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthRequest")
            .field("allow_interaction", &self.allow_interaction)
            .field("timeout_seconds", &self.timeout_seconds)
            .finish_non_exhaustive()
    }
}

impl AuthRequest {
    /// Validate selectors and bounds without inspecting accounts or configuration.
    ///
    /// # Errors
    ///
    /// Returns a fixed diagnostic without echoing any supplied value.
    pub fn validate(&self) -> Result<(), CliError> {
        let account = &self.account;
        if !guid(&self.tenant)
            || self
                .tenant
                .eq_ignore_ascii_case("9188040d-6c67-4c5b-b112-36a304b66dad")
            || !guid(&self.client_id)
            || account.len() > 320
            || account.split('@').count() != 2
            || account.starts_with('@')
            || account.ends_with('@')
            || account
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
            || !(1..=600).contains(&self.timeout_seconds)
        {
            return Err(CliError::new(
                "InvalidInput",
                "Authentication requires an explicit work-account UPN, public-cloud tenant GUID, public-client GUID and a timeout from 1 through 600 seconds.",
            ));
        }
        Ok(())
    }
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Run the explicit outer CLI adapter, creating its current-thread runtime.
///
/// The command head must already have validated the exact `/.auth` target.
/// Selectors come only from `request`; no environment, saved binding, settings,
/// picker, alternate provider or local selection store is consulted.
///
/// # Errors
///
/// Returns sanitized configuration, unsupported-platform/feature, runtime,
/// provider, cancellation, timeout or metadata failures with effect evidence.
/// This synchronous outer host must not be called from inside an async runtime;
/// embedded callers use the feature-gated `run_with_broker` seam with their own
/// runtime instead.
pub fn run(action: AuthAction, request: &AuthRequest) -> Result<OperationResult, CliError> {
    request.validate()?;
    #[cfg(all(windows, feature = "windows-broker"))]
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(CliError::new(
                "runtime_failed",
                "The synchronous authentication host cannot run inside an async runtime.",
            ));
        }
        let options = fabric_identity::LoginOptions::new(
            &request.tenant,
            &request.client_id,
            &request.account,
        )
        .map_err(|_| {
            CliError::new(
                "InvalidInput",
                "The explicit Windows broker selection is invalid.",
            )
        })?;
        let broker = WindowsBroker { options };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                CliError::new(
                    "runtime_failed",
                    "The authentication runtime could not start.",
                )
            })?;
        runtime.block_on(async {
            run_with_broker(
                action,
                request,
                &broker,
                tokio::signal::ctrl_c(),
                tokio::time::sleep(std::time::Duration::from_secs(request.timeout_seconds)),
                || {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                },
            )
            .await
        })
    }
    #[cfg(not(all(windows, feature = "windows-broker")))]
    {
        let _ = action;
        Err(CliError::new(
            "UnsupportedCapability",
            "This authentication profile requires Windows and an explicit windows-broker build; no alternate provider is attempted.",
        ))
    }
}

/// Execute the existing command engine with an explicitly injected broker.
///
/// The caller binds the broker to the validated request's account, tenant **and
/// client**; the existing broker trait exposes only account/tenant for checking.
/// Cancellation and deadline futures belong to the caller. `unix_now` is queried
/// after acquisition to validate authoritative expiry, not to guess a lifetime.
/// This seam creates no runtime and never selects a production provider.
///
/// # Errors
///
/// Invalid selectors or a mismatched broker fail before broker work. Started
/// failure/cancellation retains unknown effects; cache changes are not rollback.
/// Incomplete, reordered, expired or otherwise invalid success metadata fails
/// without publishing a partial session. Provider messages are never forwarded.
#[cfg(feature = "windows-broker")]
pub async fn run_with_broker(
    action: AuthAction,
    request: &AuthRequest,
    broker: &impl fabric_cli::auth_commands::Broker,
    cancelled: impl std::future::Future<Output = std::io::Result<()>>,
    deadline: impl std::future::Future<Output = ()>,
    unix_now: impl FnOnce() -> Option<i64>,
) -> Result<OperationResult, CliError> {
    request.validate()?;
    if !broker.tenant().eq_ignore_ascii_case(&request.tenant)
        || !broker.account().eq_ignore_ascii_case(&request.account)
    {
        return Err(CliError::new(
            "identity_mismatch",
            "The supplied broker does not match the explicitly selected identity.",
        ));
    }
    let tracked = TrackedBroker {
        broker,
        started: std::cell::Cell::new(false),
    };
    let source_action = match action {
        AuthAction::Login => fabric_cli::parser::AuthAction::Login,
        AuthAction::Logout => fabric_cli::parser::AuthAction::Logout,
    };
    let result = fabric_cli::auth_commands::run(
        source_action,
        action == AuthAction::Login && request.allow_interaction,
        &tracked,
        cancelled,
        deadline,
    )
    .await
    .map_err(|(code, _)| classified_failure(code));
    let result = result.and_then(|data| match action {
        AuthAction::Login => login_result(request, data, unix_now()),
        AuthAction::Logout => Ok(OperationResult::succeeded(serde_json::json!({
            "logged_in": false,
            "signed_out": true,
            "scope": "application_on_this_device",
            "credential_invalidation": "this_process",
            "account": request.account,
            "tenant_id": request.tenant.to_ascii_lowercase(),
            "client_id": request.client_id.to_ascii_lowercase(),
            "provider": "windows_broker",
            "selection_persisted": false,
            "already_returned_tokens_revoked": false,
            "other_processes_revoked": false,
            "os_account_removed": false,
            "browser_cookies_cleared": false,
        }))),
    });
    let kind = match action {
        AuthAction::Login => EffectKind::Authentication,
        AuthAction::Logout => EffectKind::SignOut,
    };
    result
        .map(|result| result.with_effect(kind, EffectState::Completed))
        .map_err(|mut error| {
            if tracked.started.get() {
                error = error.with_effect(kind, EffectState::Unknown);
                error.message.push_str(match action {
                    AuthAction::Login => {
                        " Broker cache changes may have occurred; cleanup is not confirmed."
                    }
                    AuthAction::Logout => {
                        " Started sign-out may have invalidated this process's old credentials; sign-out and cleanup are not confirmed."
                    }
                });
            }
            error
        })
}

#[cfg(feature = "windows-broker")]
fn login_result(
    request: &AuthRequest,
    data: serde_json::Value,
    now: Option<i64>,
) -> Result<OperationResult, CliError> {
    const SCOPES: [&str; 3] = [
        "https://api.fabric.microsoft.com/.default",
        "https://storage.azure.com/.default",
        "https://management.azure.com/.default",
    ];
    const MAX_UNIX_SECONDS: i64 = 253_402_300_799;
    let now = now
        .filter(|value| (0..=MAX_UNIX_SECONDS).contains(value))
        .ok_or_else(|| {
            CliError::new(
                "clock_unavailable",
                "The current time could not be validated after broker acquisition.",
            )
        })?;
    let invalid = || classified_failure("invalid_broker_response");
    if data["logged_in"] != true
        || !data["tenant_id"]
            .as_str()
            .is_some_and(|tenant| tenant.eq_ignore_ascii_case(&request.tenant))
        || !data["account"]
            .as_str()
            .is_some_and(|account| account.eq_ignore_ascii_case(&request.account))
    {
        return Err(classified_failure("identity_mismatch"));
    }
    let tokens = data["tokens"].as_array().ok_or_else(invalid)?;
    if tokens.len() != SCOPES.len() {
        return Err(invalid());
    }
    let mut metadata = Vec::with_capacity(SCOPES.len());
    for (token, scope) in tokens.iter().zip(SCOPES) {
        let expires_on = token["expires_on"].as_i64().ok_or_else(invalid)?;
        if token["scope"].as_str() != Some(scope)
            || expires_on <= now
            || expires_on > MAX_UNIX_SECONDS
        {
            return Err(invalid());
        }
        metadata.push(serde_json::json!({"scope": scope, "expires_on": expires_on}));
    }
    Ok(OperationResult::succeeded(serde_json::json!({
        "logged_in": true,
        "authentication_mode": "Windows broker",
        "provider": "windows_broker",
        "tenant_id": request.tenant.to_ascii_lowercase(),
        "account": request.account,
        "client_id": request.client_id.to_ascii_lowercase(),
        "tokens": metadata,
        "mfa_evidence": "not_checked",
        "service_access_verified": false,
        "selection_persisted": false,
    })))
}

#[cfg(feature = "windows-broker")]
fn classified_failure(code: &str) -> CliError {
    let (code, message) = match code {
        "invalid_configuration" | "InvalidInput" => (
            "InvalidInput",
            "The explicit broker configuration is invalid.",
        ),
        "interaction_required" => (
            "interaction_required",
            "The selected identity requires an explicit interactive login.",
        ),
        "cancelled" | "Cancelled" => ("Cancelled", "Authentication was cancelled."),
        "signal_failed" => ("signal_failed", "The cancellation handler could not start."),
        "timed_out" | "Timeout" => ("Timeout", "The authentication deadline elapsed."),
        "identity_mismatch" => (
            "identity_mismatch",
            "The broker could not establish the explicitly selected identity.",
        ),
        "invalid_broker_response" => (
            "invalid_broker_response",
            "The broker did not return valid complete three-audience metadata.",
        ),
        "broker_unavailable" => (
            "broker_unavailable",
            "The Windows broker is unavailable; no alternate provider is attempted.",
        ),
        "interactive_window_unavailable" => (
            "interactive_window_unavailable",
            "Interactive login requires an available Windows desktop window.",
        ),
        "completion_unconfirmed" => (
            "completion_unconfirmed",
            "Native broker completion is unconfirmed; do not retry in a loop.",
        ),
        "cleanup_pending" => (
            "cleanup_pending",
            "Native broker cleanup remains pending; do not retry in a loop.",
        ),
        "authentication_busy" => (
            "authentication_busy",
            "An operation for this selected application account is already active.",
        ),
        "session_invalidated" => (
            "session_invalidated",
            "The session is invalidated; finish scoped sign-out before explicit login.",
        ),
        "identity_capacity_exhausted" => (
            "identity_capacity_exhausted",
            "The process identity registry is full; restart before selecting additional identities.",
        ),
        "broker_capacity_exhausted" => (
            "broker_capacity_exhausted",
            "Broker workers remain occupied; wait for cleanup rather than retrying in a loop.",
        ),
        "coordination_failed" => (
            "coordination_failed",
            "Broker lifecycle coordination failed; restart this process.",
        ),
        _ => (
            "authentication_failed",
            "The selected broker authentication operation failed.",
        ),
    };
    CliError::new(code, message)
}

#[cfg(feature = "windows-broker")]
struct TrackedBroker<'a, B> {
    broker: &'a B,
    started: std::cell::Cell<bool>,
}

#[cfg(feature = "windows-broker")]
impl<B: fabric_cli::auth_commands::Broker> fabric_cli::auth_commands::Broker
    for TrackedBroker<'_, B>
{
    fn tenant(&self) -> &str {
        self.broker.tenant()
    }

    fn account(&self) -> &str {
        self.broker.account()
    }

    async fn login(
        &self,
        allow_interaction: bool,
    ) -> Result<Vec<fabric_cli::auth_commands::TokenMetadata>, fabric_cli::auth_commands::Failure>
    {
        self.started.set(true);
        self.broker.login(allow_interaction).await
    }

    async fn logout(&self) -> Result<(), fabric_cli::auth_commands::Failure> {
        self.started.set(true);
        self.broker.logout().await
    }
}

#[cfg(all(windows, feature = "windows-broker"))]
struct WindowsBroker {
    options: fabric_identity::LoginOptions,
}

#[cfg(all(windows, feature = "windows-broker"))]
impl fabric_cli::auth_commands::Broker for WindowsBroker {
    fn tenant(&self) -> &str {
        self.options.tenant()
    }

    fn account(&self) -> &str {
        self.options.account()
    }

    async fn login(
        &self,
        allow_interaction: bool,
    ) -> Result<Vec<fabric_cli::auth_commands::TokenMetadata>, fabric_cli::auth_commands::Failure>
    {
        let credential =
            fabric_identity::WindowsCredential::login(self.options.clone(), allow_interaction)
                .await
                .map_err(native_failure)?;
        Ok(credential
            .metadata()
            .iter()
            .map(|metadata| fabric_cli::auth_commands::TokenMetadata {
                scope: metadata.scope,
                expires_on: metadata.expires_on,
            })
            .collect())
    }

    async fn logout(&self) -> Result<(), fabric_cli::auth_commands::Failure> {
        fabric_identity::sign_out(self.options.clone())
            .await
            .map_err(native_failure)
    }
}

#[cfg(all(windows, feature = "windows-broker"))]
fn native_failure(
    error: fabric_identity::windows_broker::BrokerError,
) -> fabric_cli::auth_commands::Failure {
    use fabric_identity::windows_broker::BrokerError;
    let code = match error {
        BrokerError::InvalidConfiguration => "invalid_configuration",
        BrokerError::InteractionRequired => "interaction_required",
        BrokerError::Cancelled => "cancelled",
        BrokerError::IdentityMismatch | BrokerError::AmbiguousAccount => "identity_mismatch",
        BrokerError::InvalidResponse | BrokerError::ResponseMetadata { .. } => {
            "invalid_broker_response"
        }
        BrokerError::ProviderUnavailable => "broker_unavailable",
        BrokerError::InteractiveWindowUnavailable => "interactive_window_unavailable",
        BrokerError::CompletionUnconfirmed { .. } => "completion_unconfirmed",
        BrokerError::IdentityCapacityExhausted { .. } => "identity_capacity_exhausted",
        BrokerError::CapacityExhausted { .. } => "broker_capacity_exhausted",
        BrokerError::OperationInProgress => "authentication_busy",
        BrokerError::SessionInvalidated => "session_invalidated",
        BrokerError::CleanupPending { .. } => "cleanup_pending",
        BrokerError::TimedOut => "timed_out",
        BrokerError::CoordinationFailed => "coordination_failed",
        _ => "authentication_failed",
    };
    let error = classified_failure(code);
    (error.code, error.message)
}
