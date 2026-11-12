// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact-session admission and real native RS256/MWC/HTTP read evidence, without live I/O.

#![deny(unnameable_test_items)]

use std::process::Command;

const SESSION: &str = "/Team.Workspace/Analysis.Notebook/sessions/jupyter-1";

#[cfg(feature = "notebook-workload")]
#[path = "../../../../tests/fixtures/notebook-identity/fixture.rs"]
mod fixture;

fn binary(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_NOTEBOOK_BINDING")
        .env("FAB_RUST_FABRIC_HOST", "invalid-host-canary")
        .output()
        .unwrap()
}

#[test]
fn executable_help_and_version_do_not_capture_identity_or_binding() {
    for args in [vec!["--help"], vec!["--version"]] {
        let output = binary(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty());
    }
    let help = binary(&["--help"]);
    assert_eq!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("Notebook exact-session GET"),
        cfg!(feature = "notebook-workload")
    );
}

#[test]
fn executable_session_mount_is_opt_in_and_facade_has_the_same_local_boundary() {
    for args in [
        vec!["get", SESSION],
        vec!["artifact", "notebook", "get", SESSION],
    ] {
        let output = binary(&args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            error.contains("NotebookBindingRequired"),
            cfg!(feature = "notebook-workload")
        );
        assert!(!error.contains("invalid-host-canary"));
    }
}

#[cfg(feature = "notebook-workload")]
#[test]
fn exact_path_help_and_unsupported_options_finish_before_local_configuration() {
    for args in [
        vec!["get", SESSION, "--help"],
        vec!["artifact", "notebook", "get", SESSION, "--help"],
    ] {
        let output = binary(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Experimental exact Notebook Jupyter session GET"));
        assert!(help.contains("no active/latest lookup"));
    }
    for option in [
        vec!["--timeout", "30s"],
        vec!["--wait"],
        vec!["--project", "id"],
        vec!["--recursive"],
        vec!["--format", "raw"],
        vec!["--max-bytes", "0"],
        vec!["--unregistered"],
    ] {
        let mut args = vec!["get", SESSION];
        args.extend(option);
        let output = binary(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("InvalidRuntimeArguments"), "{error}");
        assert!(!error.contains("NotebookBindingRequired"));
    }
}

#[cfg(feature = "notebook-workload")]
mod native {
    use super::SESSION;
    use async_trait::async_trait;
    use azure_core::{
        Bytes,
        credentials::Secret,
        error::ErrorKind,
        http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers},
    };
    use fabric_cli::runtime_v2::{
        ReadCoverage, ReadEffect, ReadErrorKind, ReadOptions, ReadStatus, Representation,
        WorkloadObservation,
        host::{Destination, HostError, PendingRead, acquire, prepare_destination},
        notebook::{NotebookBinding, NotebookConnection},
        parse_args,
    };
    use fabric_identity::headless::{HeadlessCredential, HeadlessInput, ServiceIdentity};
    use fabric_sdk::{
        Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    };
    use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
    use fabric_sdk_transport::TokioTimer;
    use serde_json::{Value, json};
    use std::{
        collections::VecDeque,
        future::pending,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use super::fixture::*;

    enum Reply {
        Json(u16, Value),
        Raw(u16, Vec<u8>),
        Partial,
        Pending,
    }

    struct Wire {
        replies: Mutex<VecDeque<Reply>>,
        requests: Mutex<Vec<Request>>,
        pbi_token: String,
    }

    impl std::fmt::Debug for Wire {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Wire(<synthetic>)")
        }
    }

    #[async_trait]
    impl HttpClient for Wire {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            self.requests.lock().unwrap().push(request.clone());
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected HTTP or replay");
            let mut headers = Headers::new();
            headers.insert("content-type", "application/json");
            match reply {
                Reply::Json(status, body) => Ok(AsyncRawResponse::from_bytes(
                    status.into(),
                    headers,
                    serde_json::to_vec(&body).unwrap(),
                )),
                Reply::Raw(status, body) => {
                    Ok(AsyncRawResponse::from_bytes(status.into(), headers, body))
                }
                Reply::Partial => Ok(AsyncRawResponse::new(
                    200.into(),
                    headers,
                    Box::pin(futures_util::stream::iter([
                        Ok(Bytes::from_static(b"{")),
                        Err(azure_core::Error::with_message(
                            ErrorKind::Io,
                            "private-canary",
                        )),
                    ])),
                )),
                Reply::Pending => pending().await,
            }
        }
    }

    fn wire(reply: Reply, pbi_claims: Option<Value>) -> Arc<Wire> {
        let pbi_token = signed(
            &header("offline-key"),
            &pbi_claims.unwrap_or_else(|| claims(PBI)),
        );
        Arc::new(Wire {
            replies: Mutex::new(
                [
                    Reply::Json(
                        200,
                        json!({"access_token":token(FABRIC),"token_type":"Bearer","expires_in":3500}),
                    ),
                    Reply::Json(200, jwks("offline-key")),
                    Reply::Json(
                        200,
                        json!({"access_token":pbi_token,"token_type":"Bearer","expires_in":3500}),
                    ),
                    Reply::Json(200, json!({"backendUrl":CLUSTER})),
                    Reply::Json(200, grant()),
                    reply,
                ]
                .into(),
            ),
            requests: Mutex::new(Vec::new()),
            pbi_token,
        })
    }

    fn binding() -> Value {
        json!({
            "schemaVersion":1,
            "owner":{"workspace":"Team.Workspace","artifact":"Analysis.Notebook","kind":"notebook"},
            "environment":"production",
            "catalogOrigin":FABRIC,"clusterOrigin":CLUSTER,"workloadOrigin":WORKLOAD,
            "capacityId":CAPACITY,"workspaceId":WORKSPACE,"notebookId":NOTEBOOK,
            "account":CLIENT,"tenant":TENANT
        })
    }

    fn connect(
        wire: &Arc<Wire>,
        input: &fabric_cli::runtime_v2::ParsedInvocation,
        options: ReadOptions,
        end: Instant,
    ) -> NotebookConnection {
        let binding =
            NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
        let credential = HeadlessCredential::new_with_pbi_bootstrap(
            HeadlessInput::ClientSecret {
                identity: ServiceIdentity::new(TENANT, CLIENT).unwrap(),
                secret: Secret::new("synthetic-offline-client-secret"),
            },
            wire.clone(),
        )
        .unwrap();
        let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
        let context = RequestContext::with_endpoint(
            credential,
            CredentialSelection::new(CLIENT, TENANT, Audience::Fabric).unwrap(),
            endpoint.clone(),
        );
        let http = HttpExecutor::new(
            wire.clone(),
            Arc::new(TokioTimer),
            ExecutionOptions::default(),
        )
        .unwrap()
        .with_endpoint(endpoint);
        binding.connect(context, http, options, end).unwrap()
    }

    fn value(state: Value) -> Value {
        json!({
            "id":"jupyter-1","state":state,"name":"literal provider name","path":"/native/path",
            "kernel":{"id":"kernel-not-session","execution_state":"future","synapse_extension":{"x":null}},
            "error":{"message":"private-session-canary"},"future":{"preserve":[null,false,{"x":1}]}
        })
    }

    fn assert_routes(wire: &Wire) {
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 6);
        assert_eq!(
            requests[1].url().as_str(),
            format!("https://login.microsoftonline.com/{TENANT}/discovery/keys")
        );
        for (index, scope) in [
            (0, "api.fabric.microsoft.com"),
            (2, "analysis.windows.net%2Fpowerbi%2Fapi"),
        ] {
            assert_eq!(requests[index].method(), Method::Post);
            assert_eq!(
                requests[index].url().as_str(),
                format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token")
            );
            let body = String::from_utf8(Bytes::from(requests[index].body()).to_vec()).unwrap();
            assert!(body.contains(&format!("scope=https%3A%2F%2F{scope}%2F.default")));
            assert!(body.contains(&format!("client_id={CLIENT}")));
        }
        for index in [3, 4] {
            assert_eq!(
                requests[index]
                    .headers()
                    .get_str(&"authorization".into())
                    .unwrap(),
                format!("Bearer {}", wire.pbi_token)
            );
        }
        assert_eq!(requests[3].method(), Method::Get);
        assert_eq!(
            requests[3].url().as_str(),
            format!("{FABRIC}/metadata/cluster")
        );
        assert_eq!(requests[4].method(), Method::Post);
        assert_eq!(
            requests[4].url().as_str(),
            format!("{CLUSTER}/metadata/v201606/generatemwctoken")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&Bytes::from(requests[4].body())).unwrap(),
            json!({"capacityObjectId":CAPACITY,"workspaceObjectId":WORKSPACE,"workloadType":"Notebook","artifactObjectIds":[NOTEBOOK]})
        );
        assert_eq!(requests[5].method(), Method::Get);
        assert_eq!(
            requests[5].url().as_str(),
            format!(
                "{WORKLOAD}/webapi/capacities/{CAPACITY}/workloads/Notebook/Data/Automatic/api/workspaces/{WORKSPACE}/artifacts/{NOTEBOOK}/jupyterApi/versions/1/api/sessions/jupyter-1"
            )
        );
        assert!(requests[5].url().query().is_none());
        assert!(Bytes::from(requests[5].body()).is_empty());
        assert_eq!(
            requests[5]
                .headers()
                .get_str(&"authorization".into())
                .unwrap(),
            format!("MwcToken {MWC}")
        );
        assert_eq!(
            requests[5].headers().get_str(&"accept".into()).unwrap(),
            "application/json"
        );
    }

    async fn observe(
        input: &fabric_cli::runtime_v2::ParsedInvocation,
        wire: &Arc<Wire>,
        options: ReadOptions,
        destination: &Destination,
        end: Instant,
    ) -> Result<PendingRead, HostError> {
        let connection = connect(wire, input, options, end);
        assert!(wire.requests.lock().unwrap().is_empty());
        acquire(
            input,
            &connection.executor,
            &connection.resolver,
            options,
            destination,
            end,
            pending(),
        )
        .await
    }

    #[tokio::test]
    async fn canonical_and_facade_get_the_exact_session_preserving_failed_and_unknown_state() {
        for state in [json!("Failed"), json!({"future":"state"}), Value::Null] {
            let mut receipts = Vec::new();
            for facade in [false, true] {
                let mut args = vec!["fab-cli-v2"];
                if facade {
                    args.extend(["artifact", "notebook"]);
                }
                args.extend(["get", SESSION, "-o", "result.json"]);
                let input = parse_args(args).unwrap();
                let temp = tempfile::tempdir().unwrap();
                let end = Instant::now() + Duration::from_secs(30);
                let destination =
                    prepare_destination("result.json".into(), temp.path().into(), end, pending())
                        .await
                        .unwrap();
                let expected = value(state.clone());
                let wire = wire(Reply::Json(200, expected.clone()), None);
                let read = observe(&input, &wire, ReadOptions::default(), &destination, end)
                    .await
                    .unwrap();
                let receipt = read.deliver(destination, end, pending()).await.unwrap();
                assert_eq!(receipt.data, expected);
                assert_eq!(receipt.status, ReadStatus::ReadComplete);
                assert_eq!(receipt.effect, ReadEffect::Read);
                assert_eq!(receipt.meta.representation, Representation::NotebookSession);
                assert_eq!(receipt.meta.coverage, ReadCoverage::Resource);
                assert_eq!(receipt.meta.workload_state, Some(state.clone()));
                assert_eq!(
                    receipt.meta.workload_observation,
                    Some(WorkloadObservation::Unknown)
                );
                assert_eq!(receipt.meta.http_status, 200);
                assert_eq!(receipt.meta.continuation, None);
                let bytes = std::fs::read(temp.path().join("result.json")).unwrap();
                assert_eq!(bytes.last(), Some(&b'\n'));
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes).unwrap(),
                    serde_json::to_value(&receipt).unwrap()
                );
                assert_routes(&wire);
                assert!(wire.replies.lock().unwrap().is_empty());
                receipts.push(receipt);
            }
            assert_eq!(receipts[0], receipts[1]);
        }
    }

    #[tokio::test]
    async fn mismatched_ids_malformed_accepted_denied_partial_and_wire_overflow_are_not_reads() {
        for (reply, kind, status) in [
            (
                Reply::Json(200, json!({"id":"kernel-not-session"})),
                ReadErrorKind::InvalidResponse,
                Some(200),
            ),
            (
                Reply::Json(200, json!({"id":1})),
                ReadErrorKind::InvalidResponse,
                Some(200),
            ),
            (
                Reply::Json(200, json!({"id":"jupyter-1","kernel":{}})),
                ReadErrorKind::InvalidResponse,
                Some(200),
            ),
            (
                Reply::Json(202, value(json!("Failed"))),
                ReadErrorKind::InvalidResponse,
                Some(202),
            ),
            (
                Reply::Raw(204, Vec::new()),
                ReadErrorKind::InvalidResponse,
                Some(204),
            ),
            (
                Reply::Raw(200, b"{".to_vec()),
                ReadErrorKind::InvalidResponse,
                Some(200),
            ),
            (
                Reply::Raw(200, Vec::new()),
                ReadErrorKind::InvalidResponse,
                Some(200),
            ),
            (
                Reply::Raw(403, b"private-denied-canary".to_vec()),
                ReadErrorKind::Http,
                Some(403),
            ),
            (Reply::Partial, ReadErrorKind::Execution, None),
            (
                Reply::Raw(200, vec![b' '; 4 * 1024 * 1024 + 1]),
                ReadErrorKind::BudgetExceeded,
                None,
            ),
        ] {
            let input = parse_args(["fab-cli-v2", "get", SESSION, "-o", "result.json"]).unwrap();
            let temp = tempfile::tempdir().unwrap();
            let end = Instant::now() + Duration::from_secs(30);
            let destination = Destination::check("result.json", temp.path()).unwrap();
            let wire = wire(reply, None);
            let error = observe(&input, &wire, ReadOptions::default(), &destination, end)
                .await
                .err()
                .unwrap();
            assert_eq!(error.kind, kind);
            assert_eq!(error.http_status, status);
            assert!(!error.read_completed);
            assert!(error.receipt.is_none());
            assert!(!format!("{error:?}").contains("canary"));
            assert!(!temp.path().join("result.json").exists());
            assert_routes(&wire);
        }
    }

    #[tokio::test]
    async fn exact_data_and_receipt_bounds_are_independent_and_never_clip() {
        let expected = value(json!("future"));
        let size = serde_json::to_vec(&expected).unwrap().len();
        for (data_limit, receipt_limit, success) in [
            (size, 2 * 1024 * 1024, true),
            (size - 1, 2 * 1024 * 1024, false),
            (size, 1, false),
        ] {
            let input = parse_args([
                "fab-cli-v2",
                "get",
                SESSION,
                "-o",
                "result.json",
                "--max-bytes",
                &data_limit.to_string(),
            ])
            .unwrap();
            let temp = tempfile::tempdir().unwrap();
            let end = Instant::now() + Duration::from_secs(30);
            let destination = Destination::check("result.json", temp.path()).unwrap();
            let options = ReadOptions {
                max_receipt_bytes: receipt_limit,
                ..ReadOptions::default()
            };
            let wire = wire(Reply::Json(200, expected.clone()), None);
            let result = observe(&input, &wire, options, &destination, end).await;
            if success {
                let receipt = result
                    .unwrap()
                    .deliver(destination, end, pending())
                    .await
                    .unwrap();
                assert_eq!(receipt.data, expected);
            } else {
                assert_eq!(result.err().unwrap().kind, ReadErrorKind::BudgetExceeded);
                assert!(!temp.path().join("result.json").exists());
            }
            assert_routes(&wire);
        }
    }

    #[tokio::test]
    async fn invalid_individual_id_syntax_has_zero_credential_or_http_dispatch() {
        for id in ["bad id", "non.ascii", "kernel:1", &"x".repeat(129)] {
            let path = format!("/Team.Workspace/Analysis.Notebook/sessions/{id}");
            let input = parse_args(["fab-cli-v2", "get", &path, "-o", "result.json"]).unwrap();
            let temp = tempfile::tempdir().unwrap();
            let end = Instant::now() + Duration::from_secs(30);
            let destination = Destination::check("result.json", temp.path()).unwrap();
            let wire = wire(Reply::Json(200, value(Value::Null)), None);
            let error = observe(&input, &wire, ReadOptions::default(), &destination, end)
                .await
                .err()
                .unwrap();
            assert_eq!(error.kind, ReadErrorKind::InvalidInput);
            assert!(wire.requests.lock().unwrap().is_empty());
            assert!(!temp.path().join("result.json").exists());
        }
    }

    #[tokio::test]
    async fn signed_foreign_principal_and_wrong_pbi_audience_are_explicit_denials_before_bootstrap()
    {
        for (field, wrong) in [("oid", OTHER), ("aud", FABRIC), ("tid", OTHER)] {
            let mut pbi = claims(PBI);
            pbi[field] = json!(wrong);
            let input = parse_args(["fab-cli-v2", "get", SESSION, "-o", "result.json"]).unwrap();
            let temp = tempfile::tempdir().unwrap();
            let end = Instant::now() + Duration::from_secs(30);
            let destination = Destination::check("result.json", temp.path()).unwrap();
            let wire = wire(Reply::Json(200, value(Value::Null)), Some(pbi));
            let error = observe(&input, &wire, ReadOptions::default(), &destination, end)
                .await
                .err()
                .unwrap();
            assert_eq!(error.kind, ReadErrorKind::InvalidResponse);
            assert_eq!(wire.requests.lock().unwrap().len(), 3);
            assert!(!temp.path().join("result.json").exists());
        }
    }

    #[tokio::test]
    async fn no_clobber_preflight_and_delivery_race_never_replay_the_observation() {
        let input = parse_args(["fab-cli-v2", "get", SESSION, "-o", "result.json"]).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("result.json");
        let end = Instant::now() + Duration::from_secs(30);
        let wire = wire(Reply::Json(200, value(json!("Failed"))), None);
        std::fs::write(&path, "original").unwrap();
        assert!(
            prepare_destination("result.json".into(), temp.path().into(), end, pending())
                .await
                .is_err()
        );
        assert!(wire.requests.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        std::fs::remove_file(&path).unwrap();
        let destination = Destination::check("result.json", temp.path()).unwrap();
        let read = observe(&input, &wire, ReadOptions::default(), &destination, end)
            .await
            .unwrap();
        std::fs::write(&path, "racing owner").unwrap();
        let error = read.deliver(destination, end, pending()).await.unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::Delivery);
        assert!(error.read_completed);
        assert!(!error.file_created);
        assert_eq!(error.bytes_written, 0);
        assert_eq!(error.receipt.as_ref().unwrap().data, value(json!("Failed")));
        assert!(!format!("{error:?}").contains("private-session-canary"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "racing owner");
        assert_routes(&wire);
    }

    #[tokio::test]
    async fn original_deadline_and_cancellation_stop_pending_session_get_without_remote_actions() {
        for cancel in [false, true] {
            let input = parse_args(["fab-cli-v2", "get", SESSION, "-o", "result.json"]).unwrap();
            let temp = tempfile::tempdir().unwrap();
            let end = Instant::now() + Duration::from_secs(1);
            let destination = Destination::check("result.json", temp.path()).unwrap();
            let wire = wire(Reply::Pending, None);
            let connection = connect(&wire, &input, ReadOptions::default(), end);
            let cancelled = async {
                if !cancel {
                    pending::<()>().await;
                }
                while wire.requests.lock().unwrap().len() != 6 {
                    tokio::task::yield_now().await;
                }
            };
            let error = acquire(
                &input,
                &connection.executor,
                &connection.resolver,
                ReadOptions::default(),
                &destination,
                end,
                cancelled,
            )
            .await
            .err()
            .unwrap();
            assert_eq!(
                error.kind,
                if cancel {
                    ReadErrorKind::Cancelled
                } else {
                    ReadErrorKind::Timeout
                }
            );
            assert!(!temp.path().join("result.json").exists());
            assert_routes(&wire);
        }
    }
}
