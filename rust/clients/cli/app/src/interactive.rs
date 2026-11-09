// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Binary-private device-code composition. Libraries own no terminal/runtime.

use crate::batch_state::CommandOutcome;
use fabric_cli::{
    auth_commands::Failure,
    parser::{AuthAction, AuthCommand},
};
use fabric_cli_output::text::OutputFormat;
use fabric_identity::interactive::{
    DeviceChallenge, DeviceCodeUi, InteractiveCredential, InteractiveError, SessionStore,
};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    sync::Arc,
    time::Duration,
};

pub(crate) fn failure(error: InteractiveError) -> Failure {
    let code = match error {
        InteractiveError::Configuration => "invalid_configuration",
        InteractiveError::NotConfigured => "NotConfigured",
        InteractiveError::IdentityMismatch => "identity_mismatch",
        InteractiveError::InteractionRequired => "interaction_required",
        InteractiveError::Cancelled => "cancelled",
        InteractiveError::Expired => "expired",
        InteractiveError::Rejected => "authentication_rejected",
        InteractiveError::Unavailable => "authentication_unavailable",
        InteractiveError::Response => "invalid_token_response",
        InteractiveError::Store => "credential_store_failed",
        InteractiveError::SessionChanged => "session_invalidated",
        InteractiveError::Unsupported => "unsupported",
        InteractiveError::Scope => "invalid_scope",
        InteractiveError::Presentation => "output_failed",
    };
    (code, error.to_string())
}

struct StderrUi {
    format: OutputFormat,
}

impl DeviceCodeUi for StderrUi {
    fn present(&self, challenge: &DeviceChallenge) -> Result<(), InteractiveError> {
        let mut stderr = io::stderr().lock();
        match self.format {
            OutputFormat::Json => {
                serde_json::to_writer(
                    &mut stderr,
                    &json!({
                        "authentication": { "method": "device-code",
                            "verification_uri": challenge.verification_uri,
                            "user_code": challenge.user_code, "expires_in": challenge.expires_in
                        }
                    }),
                )
                .map_err(|_| InteractiveError::Presentation)?;
                writeln!(stderr).map_err(|_| InteractiveError::Presentation)
            }
            OutputFormat::Text => writeln!(
                stderr,
                "Open {} and enter code {} to sign in.",
                challenge.verification_uri, challenge.user_code
            )
            .map_err(|_| InteractiveError::Presentation),
        }
    }
}

fn transport() -> Result<Arc<dyn azure_core::http::HttpClient>, InteractiveError> {
    use fabric_sdk_transport::{NativeHttpClient, ProxyPolicy, TransportOptions, TrustPolicy};
    use std::num::NonZeroUsize;
    NativeHttpClient::new(TransportOptions {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(30),
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(4).expect("constant nonzero"),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    })
    .map(|http| Arc::new(http) as Arc<dyn azure_core::http::HttpClient>)
    .map_err(|_| InteractiveError::Unavailable)
}

async fn execute(command: &AuthCommand, format: OutputFormat) -> Result<Value, InteractiveError> {
    let store = super::interactive_binding::store()?;
    let options = super::interactive_binding::options(command, &store)?;
    if command.action == AuthAction::Logout {
        store.logout_selected(&options).await?;
        return Ok(json!({
            "logged_in": false, "signed_out": true, "scope": "application_on_this_device",
            "authentication_mode": "device-code", "server_tokens_revoked": false
        }));
    }
    let http = transport()?;
    let protected: Arc<dyn SessionStore> = store;
    let credential = if command.action == AuthAction::Login && command.allow_interaction {
        InteractiveCredential::login(options, http, protected, &StderrUi { format }).await?
    } else {
        InteractiveCredential::connect(options, http, protected).await?
    };
    let metadata = credential.status().await?;
    let identity = credential.identity();
    let tokens: Vec<_> = metadata
        .into_iter()
        .map(|token| {
            json!({
                "scope": token.scope, "expires_on": token.expires_on
            })
        })
        .collect();
    Ok(json!({
        "logged_in": true, "authentication_mode": "device-code",
        "tenant_id": identity.tenant, "account": identity.account,
        "object_id": identity.object_id, "tokens": tokens,
        "identity_metadata_source": "selected_https_token_endpoint",
        "id_token_signature_verified_locally": false,
        "mfa_evidence": "not_checked", "service_access_verified": false
    }))
}

pub(crate) fn run(command: &AuthCommand, format: OutputFormat) -> io::Result<CommandOutcome> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            return super::auth::write_auth_outcome_with_format(
                Err((
                    "runtime_failed",
                    "The authentication runtime could not start.".into(),
                )),
                format,
                io::stdout().lock(),
                io::stderr().lock(),
            );
        }
    };
    let result = runtime.block_on(async {
        tokio::select! {
            result = execute(command, format) => result.map_err(failure),
            signal = tokio::signal::ctrl_c() => if signal.is_ok() {
                Err(("cancelled", "Authentication cancelled. Already committed OS-store changes are not rolled back.".into()))
            } else {
                Err(("signal_failed", "The cancellation handler could not start.".into()))
            },
            () = tokio::time::sleep(Duration::from_secs(command.timeout_seconds)) => {
                Err(("timed_out", "Authentication timed out. Already committed OS-store changes are not rolled back.".into()))
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    super::auth::write_auth_outcome_with_format(
        result,
        format,
        io::stdout().lock(),
        io::stderr().lock(),
    )
}
