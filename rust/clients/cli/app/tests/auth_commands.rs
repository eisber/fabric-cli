// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root parser through the exact production command engine, with no native calls.
//!
//! The synthetic broker substitutes only the existing credential boundary. It
//! does not emulate WAM, acquire tokens or prove native acquisition ordering.

#![cfg(feature = "windows-broker")]

use fabric_cli::{
    auth_commands::{self, Broker, Failure, TokenMetadata},
    parser::{AuthCommand, Invocation, parse_args},
    session::run_auth,
};
use fabric_cli_output::text::OutputFormat;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    future::pending,
    io::{self, Write},
};
use tokio::sync::Notify;

const TENANT: &str = "11111111-2222-3333-4444-555555555555";
const CLIENT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const ACCOUNT: &str = "selected@example.test";
const SECRET_CANARY: &str = "synthetic-token-never-printed";

fn parsed(action: &str, extra: &[&str]) -> AuthCommand {
    let mut args = vec![
        "fab-rs",
        "auth",
        action,
        "--tenant",
        TENANT,
        "--client-id",
        CLIENT,
        "--account",
        ACCOUNT,
    ];
    args.extend_from_slice(extra);
    let Invocation::Auth(command) = parse_args(args).unwrap() else {
        panic!("root parser must select the auth production host");
    };
    command
}

fn metadata() -> Vec<TokenMetadata> {
    vec![
        TokenMetadata {
            scope: "https://api.fabric.microsoft.com/.default",
            expires_on: 1_700_003_600,
        },
        TokenMetadata {
            scope: "https://storage.azure.com/.default",
            expires_on: 1_700_003_601,
        },
        TokenMetadata {
            scope: "https://management.azure.com/.default",
            expires_on: 1_700_003_602,
        },
    ]
}

#[tokio::test]
async fn status_uses_the_dedicated_silent_boundary_instead_of_explicit_login() {
    struct StatusBroker;
    impl Broker for StatusBroker {
        fn tenant(&self) -> &str {
            TENANT
        }

        fn account(&self) -> &str {
            ACCOUNT
        }

        async fn login(&self, _: bool) -> Result<Vec<TokenMetadata>, Failure> {
            panic!("status must not authorize explicit login");
        }

        async fn status(&self) -> Result<Vec<TokenMetadata>, Failure> {
            Ok(metadata())
        }

        async fn logout(&self) -> Result<(), Failure> {
            panic!("status must not sign out");
        }
    }
    let result = run_auth(&parsed("status", &[]), &StatusBroker, pending(), pending())
        .await
        .unwrap();
    assert_eq!(result["tokens"].as_array().unwrap().len(), 3);
    assert_eq!(result["logged_in"], true);
}

#[derive(Debug, Eq, PartialEq)]
enum Effect {
    Login {
        tenant: String,
        client: String,
        account: String,
        allow_interaction: bool,
    },
    Logout {
        tenant: String,
        client: String,
        account: String,
    },
}

struct SyntheticBroker {
    selection: AuthCommand,
    effects: RefCell<Vec<Effect>>,
    failure: Option<Failure>,
    wait: bool,
    started: Notify,
    dropped: Cell<usize>,
    private_token: &'static str,
}

impl SyntheticBroker {
    fn new(command: &AuthCommand) -> Self {
        Self {
            selection: command.clone(),
            effects: RefCell::new(Vec::new()),
            failure: None,
            wait: false,
            started: Notify::new(),
            dropped: Cell::new(0),
            private_token: SECRET_CANARY,
        }
    }

    fn completed(&self) -> Result<(), Failure> {
        self.failure.clone().map_or(Ok(()), Err)
    }
}

struct Dropped<'a>(&'a Cell<usize>);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

impl Broker for SyntheticBroker {
    fn tenant(&self) -> &str {
        self.selection.tenant.as_deref().unwrap()
    }

    fn account(&self) -> &str {
        self.selection.account.as_deref().unwrap()
    }

    async fn login(&self, allow_interaction: bool) -> Result<Vec<TokenMetadata>, Failure> {
        let _dropped = Dropped(&self.dropped);
        self.effects.borrow_mut().push(Effect::Login {
            tenant: self.selection.tenant.clone().unwrap(),
            client: self.selection.client_id.clone(),
            account: self.selection.account.clone().unwrap(),
            allow_interaction,
        });
        self.started.notify_one();
        if self.wait {
            pending::<()>().await;
        }
        self.completed()?;
        Ok(metadata())
    }

    async fn logout(&self) -> Result<(), Failure> {
        let _dropped = Dropped(&self.dropped);
        self.effects.borrow_mut().push(Effect::Logout {
            tenant: self.selection.tenant.clone().unwrap(),
            client: self.selection.client_id.clone(),
            account: self.selection.account.clone().unwrap(),
        });
        self.started.notify_one();
        if self.wait {
            pending::<()>().await;
        }
        self.completed()
    }
}

async fn invoke(command: &AuthCommand, broker: &impl Broker) -> (u8, String, String) {
    let result = run_auth(command, broker, pending(), pending()).await;
    streams(result)
}

fn streams(result: Result<serde_json::Value, Failure>) -> (u8, String, String) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = auth_commands::write_result(result, &mut stdout, &mut stderr).unwrap();
    (
        exit,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

fn expected_login(allow_interaction: bool) -> Effect {
    Effect::Login {
        tenant: TENANT.into(),
        client: CLIENT.into(),
        account: ACCOUNT.into(),
        allow_interaction,
    }
}

#[tokio::test]
async fn every_auth_action_renders_text_and_json_from_the_same_non_secret_result() {
    for action in ["login", "status", "logout"] {
        let command = parsed(action, &[]);
        let broker = SyntheticBroker::new(&command);
        let data = run_auth(&command, &broker, pending(), pending())
            .await
            .unwrap();
        let (mut text, mut text_error) = (Vec::new(), Vec::new());
        assert_eq!(
            auth_commands::write_result_with_format(
                Ok(data.clone()),
                OutputFormat::Text,
                &mut text,
                &mut text_error,
            )
            .unwrap(),
            0
        );
        assert!(text_error.is_empty());
        let text = String::from_utf8(text).unwrap();
        assert!(!text.trim_start().starts_with('{'), "{text}");
        assert!(!text.contains(SECRET_CANARY));
        if action == "logout" {
            assert!(text.contains("Signed Out"), "{text}");
            assert!(text.contains("True"), "{text}");
            assert!(text.contains("application_on_this_device"), "{text}");
        } else {
            assert!(text.contains("Windows broker"), "{text}");
            assert!(text.contains(ACCOUNT), "{text}");
            assert!(text.contains(TENANT), "{text}");
            assert!(text.contains("not_checked"), "{text}");
            assert!(
                text.contains("https://api.fabric.microsoft.com/.default"),
                "{text}"
            );
        }
        let (mut output, mut error) = (Vec::new(), Vec::new());
        assert_eq!(
            auth_commands::write_result_with_format(
                Ok(data.clone()),
                OutputFormat::Json,
                &mut output,
                &mut error,
            )
            .unwrap(),
            0
        );
        assert!(error.is_empty());
        assert_eq!(output, format!("{data}\n").into_bytes());
    }
}

#[test]
fn auth_formats_preserve_failure_streams_codes_and_terminal_outcomes() {
    use fabric_cli::parser::batch::CommandOutcome;
    for format in [OutputFormat::Text, OutputFormat::Json] {
        for code in [
            "authentication_failed",
            "cancelled",
            "timed_out",
            "cleanup_pending",
        ] {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let outcome = auth_commands::write_outcome_with_format(
                Err((code, "Synthetic failure without credential contents".into())),
                format,
                &mut stdout,
                &mut stderr,
            )
            .unwrap();
            assert!(stdout.is_empty());
            assert_eq!(
                outcome,
                if code == "authentication_failed" {
                    CommandOutcome::Returned(1)
                } else {
                    CommandOutcome::Stop(1)
                }
            );
            let text = String::from_utf8(stderr).unwrap();
            assert!(text.contains(code), "{text}");
            if format == OutputFormat::Json {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                    json!({"error":{"code":code,"message":"Synthetic failure without credential contents"}}),
                );
            } else {
                assert!(!text.trim_start().starts_with('{'));
            }
        }
        assert!(
            auth_commands::write_result_with_format(
                Ok(json!({"signed_out":true})),
                format,
                BrokenStream,
                Vec::new(),
            )
            .is_err()
        );
        assert!(
            auth_commands::write_result_with_format(
                Err(("cancelled", "Synthetic".into())),
                format,
                Vec::new(),
                BrokenStream,
            )
            .is_err()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn login_and_status_preserve_exact_metadata_streams_and_explicit_selection() {
    for (action, extra, interactive) in [
        ("login", vec![], true),
        ("login", vec!["--no-interactive"], false),
        ("status", vec![], false),
    ] {
        let command = parsed(action, &extra);
        let broker = SyntheticBroker::new(&command);
        let (exit, stdout, stderr) = invoke(&command, &broker).await;
        let expected = json!({
            "logged_in": true,
            "authentication_mode": "Windows broker",
            "tenant_id": TENANT,
            "account": ACCOUNT,
            "tokens": [
                {"scope": "https://api.fabric.microsoft.com/.default", "expires_on": 1700003600i64},
                {"scope": "https://storage.azure.com/.default", "expires_on": 1700003601i64},
                {"scope": "https://management.azure.com/.default", "expires_on": 1700003602i64},
            ],
            "mfa_evidence": "not_checked",
            "service_access_verified": false,
        });
        assert_eq!((exit, stderr), (0, String::new()));
        assert_eq!(stdout, format!("{expected}\n"));
        assert!(!stdout.contains(broker.private_token));
        assert_eq!(*broker.effects.borrow(), [expected_login(interactive)]);
        assert_eq!(broker.dropped.get(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn status_never_authorizes_ui_even_if_a_library_caller_sets_the_flag() {
    let mut command = parsed("status", &[]);
    command.allow_interaction = true;
    let broker = SyntheticBroker::new(&command);
    assert_eq!(invoke(&command, &broker).await.0, 0);
    assert_eq!(*broker.effects.borrow(), [expected_login(false)]);
}

#[tokio::test(flavor = "current_thread")]
async fn logout_only_delegates_the_selected_application_device_effect() {
    let command = parsed("logout", &[]);
    let broker = SyntheticBroker::new(&command);
    assert_eq!(
        invoke(&command, &broker).await,
        (
            0,
            "{\"logged_in\":false,\"signed_out\":true,\"scope\":\"application_on_this_device\"}\n"
                .into(),
            String::new()
        )
    );
    assert_eq!(
        *broker.effects.borrow(),
        [Effect::Logout {
            tenant: TENANT.into(),
            client: CLIENT.into(),
            account: ACCOUNT.into(),
        }]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn broker_failures_never_publish_partial_success_retry_or_switch_provider() {
    for action in ["login", "status", "logout"] {
        for (code, message) in [
            (
                "identity_mismatch",
                "the broker returned a different account or tenant",
            ),
            (
                "interaction_required",
                "the selected identity requires interactive sign-in",
            ),
            ("cancelled", "the broker request was cancelled"),
            (
                "broker_unavailable",
                "the Windows Entra account provider is unavailable",
            ),
            (
                "invalid_broker_response",
                "the broker response lacks valid identity, token or expiry metadata",
            ),
            (
                "authentication_failed",
                "the broker rejected the request (status 5, code 0x000004d2)",
            ),
            (
                "completion_unconfirmed",
                "Windows broker completion is unconfirmed (HRESULT 0xffffffff); native cleanup remains pending",
            ),
        ] {
            let command = parsed(action, &[]);
            let mut broker = SyntheticBroker::new(&command);
            broker.failure = Some((code, message.into()));
            let (exit, stdout, stderr) = invoke(&command, &broker).await;
            assert_eq!(exit, 1);
            assert!(stdout.is_empty());
            assert_eq!(
                stderr,
                format!("{}\n", json!({"error": {"code": code, "message": message}}))
            );
            assert!(!stderr.contains(broker.private_token));
            assert!(!stderr.contains(ACCOUNT));
            assert_eq!(broker.effects.borrow().len(), 1);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_timeout_and_signal_failure_drop_pending_work_without_success() {
    for action in ["login", "status", "logout"] {
        for stop in ["cancelled", "timed_out", "signal_failed"] {
            let command = parsed(action, &[]);
            let mut broker = SyntheticBroker::new(&command);
            broker.wait = true;
            let result = run_auth(
                &command,
                &broker,
                async {
                    if stop == "timed_out" {
                        pending().await
                    } else {
                        broker.started.notified().await;
                        if stop == "signal_failed" {
                            Err(io::Error::other(SECRET_CANARY))
                        } else {
                            Ok(())
                        }
                    }
                },
                async {
                    if stop == "timed_out" {
                        broker.started.notified().await;
                    } else {
                        pending().await
                    }
                },
            )
            .await;
            let (exit, stdout, stderr) = streams(result);
            let message = match stop {
                "cancelled" => {
                    "Authentication cancelled; broker cache changes are not rolled back."
                }
                "timed_out" => {
                    "Authentication timed out; broker cleanup and cache changes may still be pending."
                }
                _ => "The cancellation handler could not start.",
            };
            assert_eq!((exit, stdout), (1, String::new()));
            assert_eq!(
                stderr,
                format!("{}\n", json!({"error": {"code": stop, "message": message}}))
            );
            assert!(!stderr.contains(SECRET_CANARY));
            assert_eq!(broker.effects.borrow().len(), 1);
            assert_eq!(broker.dropped.get(), 1);
        }
    }
}

struct BrokenStream;

impl Write for BrokenStream {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_failures_propagate_instead_of_reporting_auth_success() {
    assert!(auth_commands::write_result(Ok(json!({})), BrokenStream, Vec::new()).is_err());
    assert!(
        auth_commands::write_result(
            Err(("cancelled", "the broker request was cancelled".into())),
            Vec::new(),
            BrokenStream,
        )
        .is_err()
    );
    let mut stderr = Vec::new();
    assert_eq!(
        auth_commands::write_error(
            "invalid_configuration",
            "an explicit account, tenant GUID and client GUID are required",
            2,
            &mut stderr
        )
        .unwrap(),
        2
    );
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "{\"error\":{\"code\":\"invalid_configuration\",\"message\":\"an explicit account, tenant GUID and client GUID are required\"}}\n"
    );
}
