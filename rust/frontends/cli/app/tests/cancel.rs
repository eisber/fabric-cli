// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in root parser -> injected host -> SDK/shared HTTP -> receipt streams.
#![deny(unnameable_test_items)]

#[cfg(not(feature = "v2-cancel"))]
#[test]
fn default_grammar_does_not_mount_root_cancel() {
    assert!(
        fabric_cli::parser::command()
            .find_subcommand("cancel")
            .is_none()
    );
    assert!(fabric_cli::parser::parse_args(["fab-rs", "cancel", "/"]).is_err());
}

#[cfg(feature = "v2-cancel")]
mod enabled {
    use async_trait::async_trait;
    use azure_core::{
        credentials::{AccessToken, TokenCredential, TokenRequestOptions},
        error::ErrorKind,
        http::{AsyncRawResponse, Body, HttpClient, Method, Request, StatusCode, headers::Headers},
        time::OffsetDateTime,
    };
    use fabric_cli::{
        cancel::{Dispatch, InputError, dispatch, preflight},
        parser::{CancelCommand, Invocation, parse_args},
    };
    use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
    use fabric_sdk_jobs::{
        Audience, CredentialSelection, EndpointBinding, JobClient, JobExecutionError, JobExecutor,
        JobFuture, JobLimits, JobRequest, JobResponse, RequestContext, ServiceOrigin,
    };
    use serde_json::{Value, json};
    use std::{
        collections::VecDeque,
        future::pending,
        io::{self, Write},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
    const ITEM: &str = "22222222-2222-2222-2222-222222222222";
    const INSTANCE: &str = "33333333-3333-3333-3333-333333333333";
    const PATH: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook/runs/33333333-3333-3333-3333-333333333333";

    #[derive(Debug)]
    struct Credential {
        calls: AtomicUsize,
        deny: bool,
    }

    #[async_trait]
    impl TokenCredential for Credential {
        async fn get_token(
            &self,
            scopes: &[&str],
            _: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
            if self.deny {
                return Err(azure_core::Error::with_message(
                    ErrorKind::Credential,
                    "synthetic-credential-secret",
                ));
            }
            Ok(AccessToken::new(
                "synthetic-cancel-token",
                OffsetDateTime::UNIX_EPOCH,
            ))
        }
    }

    #[derive(Debug)]
    enum Reply {
        Response(u16, Value),
        Located(u16, Value, String),
        Lost,
        Pending,
    }

    #[derive(Debug)]
    struct Wire {
        replies: Mutex<VecDeque<Reply>>,
        requests: Mutex<Vec<Request>>,
        sent: tokio::sync::Notify,
    }

    #[async_trait]
    impl HttpClient for Wire {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            self.requests.lock().unwrap().push(request.clone());
            self.sent.notify_one();
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected HTTP");
            let (status, value, location) = match reply {
                Reply::Response(status, value) => (status, value, None),
                Reply::Located(status, value, location) => (status, value, Some(location)),
                Reply::Lost => {
                    return Err(azure_core::Error::with_message(
                        ErrorKind::Io,
                        "synthetic-transport-secret",
                    ));
                }
                Reply::Pending => return pending().await,
            };
            let mut headers = Headers::new();
            headers.insert("x-ms-request-id", "offline-request-1");
            headers.insert("x-provider-message", "synthetic-provider-header-secret");
            headers.insert("retry-after", "1");
            if let Some(location) = location {
                headers.insert("location", location);
            }
            Ok(AsyncRawResponse::from_bytes(
                StatusCode::from(status),
                headers,
                serde_json::to_vec(&value).unwrap(),
            ))
        }
    }

    struct Timer;
    impl ExecutionTimer for Timer {
        fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
            Box::pin(pending())
        }
    }

    fn host(reply: Reply, deny: bool) -> (JobClient, Arc<Credential>, Arc<Wire>) {
        let credential = Arc::new(Credential {
            calls: AtomicUsize::new(0),
            deny,
        });
        let wire = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([reply])),
            requests: Mutex::new(Vec::new()),
            sent: tokio::sync::Notify::new(),
        });
        let endpoint =
            EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com")
                .unwrap();
        let executor =
            HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
                .unwrap()
                .with_endpoint(endpoint.clone());
        let context = RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline-account", WORKSPACE, Audience::Fabric).unwrap(),
            endpoint,
        );
        (
            JobClient::with_context(
                context,
                Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor)),
                JobLimits::default(),
            ),
            credential,
            wire,
        )
    }

    fn command(extra: &[&str]) -> CancelCommand {
        let Invocation::Cancel(command) = parse_args(
            ["fab-rs", "cancel", PATH]
                .into_iter()
                .chain(extra.iter().copied()),
        )
        .unwrap() else {
            panic!("root cancel invocation")
        };
        command
    }

    fn streams(delivery: &Dispatch) -> (u8, Value) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = delivery.write_to(&mut stdout, &mut stderr);
        let bytes = if exit == 0 {
            assert!(stderr.is_empty());
            stdout
        } else {
            assert!(stdout.is_empty());
            stderr
        };
        assert_eq!(bytes.last(), Some(&b'\n'));
        let text = String::from_utf8(bytes).unwrap();
        for secret in [
            "synthetic-cancel-token",
            "synthetic-credential-secret",
            "synthetic-transport-secret",
            "synthetic-provider-header-secret",
        ] {
            assert!(!text.contains(secret));
        }
        (exit, serde_json::from_str(&text).unwrap())
    }

    fn assert_handle(value: &Value) {
        assert_eq!(
            value["data"],
            json!({
                "kind":"itemJobRun", "workspaceId":WORKSPACE, "itemId":ITEM, "instanceId":INSTANCE,
            })
        );
        assert_eq!(value["meta"]["target"]["path"], PATH);
        assert_eq!(
            value["meta"]["target"]["endpoint"],
            "https://dailyapi.fabric.microsoft.com/"
        );
        assert_eq!(value["meta"]["effects"]["completeness"], "unknown");
        assert_eq!(value["meta"]["cancellation"]["confirmed"], false);
    }

    #[tokio::test]
    async fn acceptance_is_one_exact_post_with_no_status_operation_or_terminal_claim() {
        for body in [json!({"status":"Cancelled"}), Value::Null] {
            let (client, credential, wire) = host(Reply::Response(202, body), false);
            let result = dispatch(&command(&["--format", "json"]), &client, pending()).await;
            let (exit, value) = streams(&result);
            assert_eq!(exit, 0);
            assert_eq!(value["schemaVersion"], "0.1");
            assert_eq!(value["status"], "accepted");
            assert_eq!(value["error"], Value::Null);
            assert_eq!(value["meta"]["cancellation"]["requested"], true);
            assert_eq!(value["meta"]["mutation"]["accepted"], true);
            assert_eq!(value["meta"]["mutation"]["responseStatus"], 202);
            assert_eq!(value["meta"]["mutation"]["responseBodyOmitted"], false);
            assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
            assert_handle(&value);
            assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
            let requests = wire.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method(), Method::Post);
            assert_eq!(
                requests[0].url().as_str(),
                format!(
                    "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items/{ITEM}/jobs/instances/{INSTANCE}/cancel",
                )
            );
            assert!(matches!(requests[0].body(), Body::Bytes(bytes) if bytes.is_empty()));
            assert!(wire.replies.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn only_202_is_acceptance_even_if_another_response_claims_a_terminal_state() {
        for status in [200, 201, 204] {
            let (client, _, wire) = host(
                Reply::Response(status, json!({"status":"Cancelled"})),
                false,
            );
            let (exit, value) = streams(&dispatch(&command(&[]), &client, pending()).await);
            assert_eq!(exit, 1);
            assert_handle(&value);
            assert_eq!(value["status"], "error");
            assert_eq!(
                value["error"]["source"]["classification"],
                "UnexpectedStatus"
            );
            assert_eq!(value["error"]["source"]["details"]["httpStatus"], status);
            assert_eq!(
                value["error"]["correlation"]["x-ms-request-id"],
                "offline-request-1"
            );
            assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
            assert_eq!(value["meta"]["cancellation"]["requested"], Value::Null);
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn supplied_location_is_validated_against_the_bound_instance_and_never_followed() {
        let bound = format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items/{ITEM}/jobs/instances/{INSTANCE}",
        );
        for (location, accepted) in [
            (bound.clone(), true),
            (bound.replace("dailyapi.", "api."), false),
            (bound.replace(INSTANCE, WORKSPACE), false),
            (
                "https://dailyapi.fabric.microsoft.com/v1/operations/opaque".into(),
                false,
            ),
        ] {
            let (client, _, wire) = host(Reply::Located(202, Value::Null, location), false);
            let (exit, value) = streams(&dispatch(&command(&[]), &client, pending()).await);
            assert_eq!(exit, if accepted { 0 } else { 1 });
            assert_handle(&value);
            assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
            if !accepted {
                assert_eq!(value["status"], "error");
                assert_eq!(value["meta"]["cancellation"]["requested"], Value::Null);
                assert_eq!(value["meta"]["mutation"]["responseStatus"], 202);
                assert_eq!(value["meta"]["mutation"]["responseBodyOmitted"], false);
                assert_eq!(
                    value["error"]["source"]["classification"],
                    "InvalidResponse"
                );
                assert_eq!(
                    value["error"]["correlation"]["x-ms-request-id"],
                    "offline-request-1"
                );
            }
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn malformed_paths_other_types_and_formats_fail_before_credentials_and_http() {
        let (client, credential, wire) = host(Reply::Lost, false);
        for (path, expected) in [
            (
                "relative.Workspace/book.Notebook/runs/run".into(),
                InputError::InvalidInput,
            ),
            (
                PATH.replace(INSTANCE, "not-a-guid"),
                InputError::InvalidInput,
            ),
            (
                PATH.replace(INSTANCE, "333333333333333333333333333333333333"),
                InputError::InvalidInput,
            ),
            (
                PATH.replace(ITEM, "２2222222-2222-2222-2222-222222222222"),
                InputError::InvalidInput,
            ),
            (
                PATH.replace("Notebook", "DataPipeline"),
                InputError::UnsupportedCapability,
            ),
            (
                PATH.replace("runs", "sessions"),
                InputError::UnsupportedCapability,
            ),
            (format!("{PATH}/extra"), InputError::InvalidInput),
            (format!("{PATH}?query=x"), InputError::InvalidInput),
            (PATH.replace(ITEM, "*"), InputError::InvalidInput),
        ] {
            let mut command = command(&[]);
            command.path = path;
            assert_eq!(preflight(&command), Err(expected));
            let (exit, value) = streams(&dispatch(&command, &client, pending()).await);
            assert_eq!(exit, 2);
            assert_eq!(value["error"]["code"], expected.code());
            assert_eq!(value["meta"]["mutationMayHaveOccurred"], false);
        }
        for extra in [
            vec!["--format", "text"],
            vec!["--format", "yaml"],
            vec!["--output_format", "text"],
            vec!["--output_format", "json"],
        ] {
            let result = dispatch(&command(&extra), &client, pending()).await;
            assert_eq!(
                streams(&result).1["error"]["code"],
                "explicitFormatNotSupported"
            );
        }
        let Invocation::Cancel(before_root) =
            parse_args(["fab-rs", "--output_format", "text", "cancel", PATH]).unwrap()
        else {
            panic!("root cancel")
        };
        assert_eq!(
            preflight(&before_root),
            Err(InputError::ExplicitFormatNotSupported)
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(wire.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn grammar_rejects_wait_timeout_bulk_and_extra_input_but_preserves_v1_cancel() {
        for extra in [
            vec!["--wait"],
            vec!["--timeout", "1"],
            vec!["--input", "{}"],
            vec!["--all"],
            vec![PATH],
            vec!["--format", "json", "--format", "json"],
        ] {
            assert!(
                parse_args(
                    ["fab-rs", "cancel", PATH]
                        .into_iter()
                        .chain(extra.iter().copied()),
                )
                .is_err(),
                "unexpectedly accepted cancel arguments: {extra:?}",
            );
        }
        assert!(parse_args(["fab-rs", "-c", "ls", "cancel", PATH]).is_err());
        assert!(matches!(
            parse_args([
                "fab-rs",
                "job",
                "run-cancel",
                "Book.Notebook",
                "--id",
                "run"
            ])
            .unwrap(),
            Invocation::JobMutation(_),
        ));
    }

    #[tokio::test]
    async fn credential_failure_preserves_handle_and_no_mutation_evidence() {
        let (client, credential, wire) = host(Reply::Lost, true);
        let (exit, value) = streams(&dispatch(&command(&[]), &client, pending()).await);
        assert_eq!(exit, 1);
        assert_handle(&value);
        assert_eq!(value["error"]["source"]["classification"], "Credential");
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], false);
        assert_eq!(value["meta"]["mutation"]["responseStatus"], Value::Null);
        assert_eq!(
            value["meta"]["mutation"]["responseBodyOmitted"],
            Value::Null
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert!(wire.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn service_error_projects_only_fixed_bounded_fields_not_provider_property_names() {
        let (client, _, wire) = host(
            Reply::Response(
                403,
                json!({
                    "errorCode":"Denied", "message":"synthetic-cancel-token",
                    "requestId":"offline-request-1", "future":{"access_token":"synthetic-cancel-token","retry":false},
                    "isRetriable":false,
                    "synthetic-provider-key-secret":{"code":"synthetic-hidden-code-secret"},
                    "error":{
                        "errorCode":"NestedDenied", "correlationId":"offline-correlation-1",
                        "synthetic-nested-key-secret":"hidden",
                        "error":{"code":"synthetic-depth-secret"},
                    },
                    "unknownRows":vec![json!({"synthetic-array-key-secret":"hidden"}); 100],
                }),
            ),
            false,
        );
        let (exit, value) = streams(&dispatch(&command(&[]), &client, pending()).await);
        assert_eq!(exit, 1);
        assert_handle(&value);
        assert_eq!(value["error"]["code"], "ServiceError");
        assert_eq!(value["error"]["source"]["details"]["httpStatus"], 403);
        assert_eq!(
            value["error"]["serviceError"],
            json!({
                "errorCode":"Denied", "requestId":"offline-request-1", "isRetriable":false,
                "error":{"errorCode":"NestedDenied","correlationId":"offline-correlation-1"},
            })
        );
        assert!(value["error"].get("rawServiceError").is_none());
        let output = serde_json::to_string(&value).unwrap();
        for secret in [
            "synthetic-provider-key-secret",
            "synthetic-hidden-code-secret",
            "synthetic-nested-key-secret",
            "synthetic-depth-secret",
            "synthetic-array-key-secret",
        ] {
            assert!(!output.contains(secret));
        }
        assert!(output.len() < 2048);
        assert_eq!(
            value["error"]["correlation"]["x-ms-request-id"],
            "offline-request-1"
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn throttling_and_transient_errors_never_replay_a_mutation() {
        for status in [429, 503] {
            let (client, _, wire) = host(
                Reply::Response(
                    status,
                    json!({
                        "errorCode":"Unavailable", "message":"untrusted",
                    }),
                ),
                false,
            );
            let (exit, value) = streams(&dispatch(&command(&[]), &client, pending()).await);
            assert_eq!(exit, 1);
            assert_handle(&value);
            assert_eq!(value["error"]["source"]["details"]["httpStatus"], status);
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }

    struct OversizedResponse(AtomicUsize);
    impl JobExecutor for OversizedResponse {
        fn execute<'a>(
            &'a self,
            request: JobRequest<'a>,
        ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.method(), Method::Post);
            assert!(request.url().path().ends_with("/cancel"));
            Box::pin(async {
                Ok(JobResponse {
                    status: 202,
                    body: b"synthetic-oversized-response-secret".to_vec(),
                    headers: std::collections::BTreeMap::from([(
                        "x-ms-request-id".into(),
                        "offline-response-limit".into(),
                    )]),
                })
            })
        }

        fn wait(&self, _: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
            panic!("Cancellation must not wait")
        }
    }

    #[tokio::test]
    async fn omitted_response_body_is_reported_as_omitted_not_complete_empty_evidence() {
        let (base, credential, wire) = host(Reply::Lost, false);
        let executor = Arc::new(OversizedResponse(AtomicUsize::new(0)));
        let client = JobClient::with_context(
            base.context().clone(),
            executor.clone(),
            JobLimits {
                max_response_bytes: std::num::NonZeroUsize::new(16).unwrap(),
                ..JobLimits::default()
            },
        );
        let (exit, value) = streams(&dispatch(&command(&[]), &client, pending()).await);
        assert_eq!(exit, 1);
        assert_handle(&value);
        assert_eq!(value["error"]["source"]["classification"], "ResponseLimit");
        assert_eq!(value["meta"]["mutation"]["responseStatus"], 202);
        assert_eq!(value["meta"]["mutation"]["responseBodyOmitted"], true);
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(
            value["error"]["correlation"]["x-ms-request-id"],
            "offline-response-limit"
        );
        assert!(
            !serde_json::to_string(&value)
                .unwrap()
                .contains("synthetic-oversized-response-secret")
        );
        assert_eq!(executor.0.load(Ordering::SeqCst), 1);
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert!(wire.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn response_loss_and_cancellation_retain_scoped_handle_without_replay() {
        let (client, _, wire) = host(Reply::Lost, false);
        let (exit, value) = streams(&dispatch(&command(&[]), &client, pending()).await);
        assert_eq!(exit, 1);
        assert_handle(&value);
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(value["meta"]["mutation"]["responseStatus"], Value::Null);
        assert_eq!(
            value["meta"]["mutation"]["responseBodyOmitted"],
            Value::Null
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);

        let (client, _, wire) = host(Reply::Pending, false);
        let cancelled = async { wire.sent.notified().await };
        let (exit, value) = streams(&dispatch(&command(&[]), &client, cancelled).await);
        assert_eq!(exit, 1);
        assert_handle(&value);
        assert_eq!(value["error"]["source"]["classification"], "Cancelled");
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(value["meta"]["mutation"]["responseStatus"], Value::Null);
        assert_eq!(
            value["meta"]["mutation"]["responseBodyOmitted"],
            Value::Null
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }

    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic-output-secret",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn output_failure_keeps_accepted_handle_in_stderr_and_never_replays() {
        let (client, _, wire) = host(Reply::Response(202, Value::Null), false);
        let result = dispatch(&command(&[]), &client, pending()).await;
        let mut stderr = Vec::new();
        assert_eq!(result.write_to(&mut BrokenWriter, &mut stderr), 1);
        let value: Value = serde_json::from_slice(&stderr).unwrap();
        assert_handle(&value);
        assert_eq!(value["error"]["code"], "OutputFailure");
        assert_eq!(value["meta"]["mutation"]["accepted"], true);
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }

    #[derive(Default)]
    struct FlushFailure(Vec<u8>);
    impl Write for FlushFailure {
        fn write(&mut self, value: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(value);
            Ok(value.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic-output-secret",
            ))
        }
    }

    #[tokio::test]
    async fn flush_failure_is_nonzero_and_retains_acceptance_evidence() {
        let (client, _, wire) = host(Reply::Response(202, Value::Null), false);
        let result = dispatch(&command(&[]), &client, pending()).await;
        let mut stdout = FlushFailure::default();
        let mut stderr = Vec::new();
        assert_eq!(result.write_to(&mut stdout, &mut stderr), 1);
        let accepted: Value = serde_json::from_slice(&stdout.0).unwrap();
        assert_eq!(accepted["status"], "accepted");
        let failure: Value = serde_json::from_slice(&stderr).unwrap();
        assert_handle(&failure);
        assert_eq!(failure["error"]["code"], "OutputFailure");
        assert_eq!(failure["meta"]["mutation"]["accepted"], true);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }

    #[cfg(not(feature = "native-host"))]
    #[test]
    fn opt_in_binary_without_host_is_truthful_and_preflights_before_host_selection() {
        for (path, format, exit, code) in [
            (PATH, "json", 1, "NotConfigured"),
            (PATH, "text", 2, "explicitFormatNotSupported"),
            ("/invalid", "json", 2, "InvalidInput"),
        ] {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_fab-rs"))
                .args(["cancel", path, "--format", format])
                .env(
                    "FAB_RUST_CONFIG_ROOT",
                    "relative-cancel-root-must-not-be-resolved",
                )
                .env("FAB_RUST_CONTEXT", "not-a-context")
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(exit));
            assert!(output.stdout.is_empty());
            let value: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(value["error"]["code"], code);
            assert_eq!(value["meta"]["mutationMayHaveOccurred"], false);
            if code == "NotConfigured" {
                assert_eq!(value["data"]["instanceId"], INSTANCE);
            }
        }
    }
}
