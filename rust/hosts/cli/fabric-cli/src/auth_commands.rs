// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Authentication command orchestration over an explicitly configured broker.
//!
//! The caller owns configuration validation, the runtime and cancellation/timeout
//! futures. The production adapter delegates to the existing Windows credential;
//! this module does not acquire tokens, choose accounts or implement a provider.

use super::parser::AuthAction;
use serde_json::{Value, json};
use std::{
    future::Future,
    io::{self, Write},
};

/// A classified, sanitized failure from the configured broker.
///
/// The message must not contain tokens, provider response text or supplied
/// invalid configuration. The production adapter uses its existing error mapper.
pub type Failure = (&'static str, String);

/// Non-secret metadata returned only after the existing login operation succeeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenMetadata {
    /// Exact requested scope, not an access token.
    pub scope: &'static str,
    /// Broker-derived expiry in Unix seconds.
    pub expires_on: i64,
}

/// The existing login/sign-out boundary for one validated, explicit selection.
///
/// Implementations must not substitute a different account, tenant, client or
/// provider. Login owns the existing Fabric/OneLake/ARM acquisition sequence and
/// its identity checks; the CLI neither duplicates nor replaces that engine.
/// Only successful, non-secret metadata crosses this command boundary.
pub trait Broker {
    /// The validated target tenant, not a discovered or inferred tenant.
    fn tenant(&self) -> &str;

    /// The explicitly selected work account.
    fn account(&self) -> &str;

    /// Delegate the complete existing three-audience login operation.
    ///
    /// `false` forbids UI, including during silent refresh. Partial acquisition
    /// must return a failure rather than publish incomplete metadata.
    ///
    /// # Errors
    ///
    /// Returns the existing provider's classified, sanitized failure.
    fn login(
        &self,
        allow_interaction: bool,
    ) -> impl Future<Output = Result<Vec<TokenMetadata>, Failure>>;

    /// Sign out only this application's selected account on this device.
    ///
    /// # Errors
    ///
    /// Returns the existing provider's classified, sanitized failure.
    fn logout(&self) -> impl Future<Output = Result<(), Failure>>;
}

/// Run the production auth command path without owning a runtime or OS signal.
///
/// `cancelled` has the same result contract as the executable's Ctrl+C future.
/// Dropping pending broker work requests its existing cooperative cleanup, not
/// rollback. Status deliberately uses the same acquisition path as login, with
/// interaction disabled.
///
/// # Errors
///
/// Preserves broker failures and reports cancellation, timeout or signal setup
/// failure without publishing a successful or partial result.
pub async fn run(
    action: AuthAction,
    allow_interaction: bool,
    broker: &impl Broker,
    cancelled: impl Future<Output = io::Result<()>>,
    deadline: impl Future<Output = ()>,
) -> Result<Value, Failure> {
    tokio::select! {
        result = execute(action, allow_interaction, broker) => result,
        signal = cancelled => {
            if signal.is_ok() {
                Err(("cancelled", "Authentication cancelled; broker cache changes are not rolled back.".to_owned()))
            } else {
                Err(("signal_failed", "The cancellation handler could not start.".to_owned()))
            }
        },
        () = deadline => {
            Err(("timed_out", "Authentication timed out; broker cleanup and cache changes may still be pending.".to_owned()))
        },
    }
}

async fn execute(
    action: AuthAction,
    allow_interaction: bool,
    broker: &impl Broker,
) -> Result<Value, Failure> {
    if action == AuthAction::Logout {
        broker.logout().await?;
        return Ok(
            json!({"logged_in": false, "signed_out": true, "scope": "application_on_this_device"}),
        );
    }
    let tokens: Vec<Value> = broker
        .login(action == AuthAction::Login && allow_interaction)
        .await?
        .iter()
        .map(|metadata| {
            json!({
                "scope": metadata.scope, "expires_on": metadata.expires_on,
            })
        })
        .collect();
    Ok(json!({
        "logged_in": true, "authentication_mode": "Windows broker",
        "tenant_id": broker.tenant(), "account": broker.account(), "tokens": tokens,
        "mfa_evidence": "not_checked", "service_access_verified": false,
    }))
}

/// Write the unchanged auth result envelope to exactly one command stream.
///
/// Returns exit 0 only for successful completion, otherwise exit 1.
///
/// # Errors
///
/// Propagates output failures to the executable's existing output-failure path.
pub fn write_result(
    result: Result<Value, Failure>,
    stdout: impl Write,
    stderr: impl Write,
) -> io::Result<u8> {
    match result {
        Ok(data) => {
            write_json(stdout, &data)?;
            Ok(0)
        }
        Err((code, message)) => write_error(code, &message, 1, stderr),
    }
}

/// Write a sanitized configuration, runtime or authentication failure.
///
/// # Errors
///
/// Propagates serialization and stream failures without reporting success.
pub fn write_error(
    code: &'static str,
    message: &str,
    exit: u8,
    stderr: impl Write,
) -> io::Result<u8> {
    write_json(
        stderr,
        &json!({"error": {"code": code, "message": message}}),
    )?;
    Ok(exit)
}

fn write_json(mut stream: impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut stream, value).map_err(io::Error::other)?;
    writeln!(stream)
}
