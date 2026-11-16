// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic mocked transport only. No ambient credentials or live services.
#![cfg(feature = "maps")]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::maps::{execution, export_table, parse, render};
use fabric_cli_output::tabular::{TabularFormat, encode};
use fabric_sdk::maps::{POWER_BI_SCOPE, ProcessingResult};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const BEARER: &str = "synthetic-entra-not-a-credential";
const SAS: &str = "synthetic-sas-not-a-credential";

#[derive(Debug)]
struct Credential {
    calls: AtomicUsize,
    expired: bool,
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [POWER_BI_SCOPE]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let expiry =
            OffsetDateTime::now_utc().unix_timestamp() + if self.expired { -60 } else { 3600 };
        Ok(AccessToken::new(
            BEARER,
            OffsetDateTime::from_unix_timestamp(expiry).unwrap(),
        ))
    }
}

#[derive(Debug)]
struct Wire {
    responses: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    observed: Mutex<Vec<(Method, String, String)>>,
    suspend_provider: bool,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let authorization = request
            .headers()
            .iter()
            .find(|(k, _)| k.as_str().eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str().to_owned())
            .unwrap();
        let count = {
            let mut observed = self.observed.lock().unwrap();
            observed.push((
                request.method(),
                request.url().as_str().to_owned(),
                authorization,
            ));
            observed.len()
        };
        assert!(
            !request
                .headers()
                .iter()
                .any(|(name, _)| matches!(name.as_str(), "subscription-key" | "x-ms-client-id"))
        );
        if request.method() == Method::Post {
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|(name, value)| name.as_str() == "content-type"
                        && value.as_str() == "application/geo+json")
            );
        }
        if self.suspend_provider && count == 3 {
            return std::future::pending().await;
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("No automatic extra request or replay")
    }
}

fn response(status: u16, body: Value) -> azure_core::Result<AsyncRawResponse> {
    Ok(AsyncRawResponse::from_bytes(
        status.into(),
        Headers::new(),
        serde_json::to_vec(&body).unwrap(),
    ))
}
fn command(args: &[&str]) -> fabric_cli::maps::MapsCommand {
    parse(args.iter().map(OsString::from)).unwrap()
}
fn wire(last: azure_core::Result<AsyncRawResponse>) -> Arc<Wire> {
    Arc::new(Wire {
        responses: Mutex::new(VecDeque::from([
            response(
                200,
                json!({"backendUrl":"https://synthetic.analysis.windows.net/"}),
            ),
            response(200, json!({"accountSasToken":SAS})),
            last,
        ])),
        observed: Mutex::new(Vec::new()),
        suspend_provider: false,
    })
}
fn credential() -> Arc<Credential> {
    Arc::new(Credential {
        calls: AtomicUsize::new(0),
        expired: false,
    })
}
fn synthetic_weather() -> Value {
    json!({"results":[{"dateTime":"2026-11-15T17:47:00-05:00","phrase":"Synthetic only","temperature":{"value":8.3,"unit":"C","unitType":17},"nullable":null}]})
}
async fn run(
    command: &fabric_cli::maps::MapsCommand,
    credential: Arc<Credential>,
    wire: Arc<Wire>,
) -> Result<ProcessingResult, execution::Failure> {
    execution::execute(
        &command.request,
        true,
        Duration::from_secs(2),
        credential,
        wire,
    )
    .await
}

#[test]
fn all_options_validate_before_any_host_or_transport_effects() {
    for args in [
        vec!["current", "--coordinates", "NaN,40"],
        vec!["reverse", "--coordinates", "0,91"],
        vec!["geocode", "--address", "x", "--top", "0"],
        vec!["geocode", "--address", "x", "--address-line", "y"],
        vec!["geocode", "--address-line", "y"],
        vec!["dailyforecast", "--coordinates", "0,0", "--duration", "2"],
        vec![
            "dailyhistoricalweather",
            "--coordinates",
            "0,0",
            "--start-date",
            "2026-02-29",
            "--end-date",
            "2026-03-01",
        ],
        vec!["directions", "--point", "0,0"],
        vec![
            "matrix",
            "--origin",
            "0,0",
            "--destination",
            "1,1",
            "--traffic",
            "live",
        ],
        vec!["geocode", "--address", "x", "--timeout", "121"],
        vec!["geocode", "--address", "x", "--timeout", "0"],
        vec!["geocode", "--address", "x", "--query", "[?"],
        vec!["geocode", "--address", "x", "--output", "x"],
        vec![
            "geocode",
            "--address",
            "x",
            "--format",
            "csv",
            "--output",
            "x",
            "--query",
            "features",
        ],
    ] {
        assert!(parse(args.iter().map(OsString::from)).is_err(), "{args:?}");
    }
    let c = command(&[
        "current",
        "--coordinates",
        "-73.9632,40.7794",
        "--units",
        "imperial",
        "--details",
    ]);
    assert_eq!(c.request.context()["units"], "imperial");
    assert_eq!(c.request.context()["details"], true);
    let help = parse([OsString::from("--help")]).unwrap_err();
    assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    assert!(help.to_string().contains("private"));
}

#[tokio::test]
async fn opt_in_and_expiry_fail_before_network() {
    let c = command(&["current", "--coordinates", "0,0"]);
    let credential = credential();
    let wire = wire(response(200, synthetic_weather()));
    assert_eq!(
        execution::execute(
            &c.request,
            false,
            c.timeout,
            credential.clone(),
            wire.clone()
        )
        .await
        .unwrap_err()
        .code,
        "PrivateContractOptInRequired"
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(wire.observed.lock().unwrap().is_empty());
    let expired = Arc::new(Credential {
        calls: AtomicUsize::new(0),
        expired: true,
    });
    assert_eq!(
        run(&c, expired, wire.clone()).await.unwrap_err().code,
        "CredentialExpired"
    );
    assert!(wire.observed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn exact_three_request_auth_boundaries_use_header_only_and_preserve_weather() {
    let c = command(&["current", "--coordinates", "-73.9632,40.7794"]);
    let wire = wire(response(200, synthetic_weather()));
    let result = run(&c, credential(), wire.clone()).await.unwrap();
    let observed = wire.observed.lock().unwrap();
    assert_eq!(observed.len(), 3);
    assert_eq!(observed[0].1, "https://api.powerbi.com/metadata/cluster");
    assert_eq!(
        observed[1].1,
        "https://synthetic.analysis.windows.net/metadata/v202212/azureMapToken"
    );
    assert!(
        observed[2]
            .1
            .starts_with("https://atlas.microsoft.com/weather/currentConditions/json?")
    );
    assert_eq!(observed[0].2, format!("Bearer {BEARER}"));
    assert_eq!(observed[1].2, format!("Bearer {BEARER}"));
    assert_eq!(observed[2].2, format!("jwt-sas {SAS}"));
    for (_, url, _) in observed.iter() {
        assert!(!url.contains(BEARER));
        assert!(!url.contains(SAS));
    }
    assert_eq!(result.document()["data"], synthetic_weather());
    assert!(result.complete());
    let text = String::from_utf8(render(&result, None, true, false).unwrap()).unwrap();
    assert!(text.contains("2026-11-15T17:47:00-05:00"));
    assert!(text.contains("8.3"));
}

#[tokio::test]
async fn hostile_discovery_and_redirects_never_receive_forwarded_credentials() {
    let c = command(&["current", "--coordinates", "0,0"]);
    for backend in [
        "https://attacker.test",
        "http://wabi.analysis.windows.net",
        "https://wabi.analysis.windows.net@attacker.test",
    ] {
        let wire = Arc::new(Wire {
            responses: Mutex::new(VecDeque::from([response(
                200,
                json!({"backendUrl":backend}),
            )])),
            observed: Mutex::new(Vec::new()),
            suspend_provider: false,
        });
        assert_eq!(
            run(&c, credential(), wire.clone()).await.unwrap_err().code,
            "UntrustedOrigin"
        );
        assert_eq!(wire.observed.lock().unwrap().len(), 1);
    }
    let wire = Arc::new(Wire {
        responses: Mutex::new(VecDeque::from([response(302, json!({}))])),
        observed: Mutex::new(Vec::new()),
        suspend_provider: false,
    });
    assert_eq!(
        run(&c, credential(), wire.clone()).await.unwrap_err().code,
        "UntrustedRedirect"
    );
    assert_eq!(wire.observed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unauthorized_unsupported_throttled_and_uncertain_posts_are_never_replayed() {
    let c = command(&["directions", "--point", "0,0", "--point", "1,1"]);
    for (status, code) in [
        (401, "CredentialExpiredOrRefused"),
        (403, "AdmissionDenied"),
        (404, "UnsupportedContract"),
        (429, "Throttled"),
        (500, "ProviderFailed"),
    ] {
        let wire = wire(response(
            status,
            json!({"error":{"message":format!("do not expose {BEARER} {SAS}")}}),
        ));
        let error = run(&c, credential(), wire.clone()).await.unwrap_err();
        assert_eq!(error.code, code);
        assert!(error.post_may_have_been_sent);
        assert_eq!(wire.observed.lock().unwrap().len(), 3);
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains(BEARER));
        assert!(!serialized.contains(SAS));
    }
    let wire = wire(Err(azure_core::Error::with_message(
        azure_core::error::ErrorKind::Io,
        "synthetic transport loss",
    )));
    assert!(
        run(&c, credential(), wire.clone())
            .await
            .unwrap_err()
            .post_may_have_been_sent
    );
    assert_eq!(wire.observed.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn total_deadline_retains_post_uncertainty_and_no_replay() {
    let c = command(&["directions", "--point", "0,0", "--point", "1,1"]);
    let wire = Arc::new(Wire {
        responses: Mutex::new(VecDeque::from([
            response(
                200,
                json!({"backendUrl":"https://synthetic.analysis.windows.net/"}),
            ),
            response(200, json!({"accountSasToken":SAS})),
        ])),
        observed: Mutex::new(Vec::new()),
        suspend_provider: true,
    });
    let error = execution::execute(
        &c.request,
        true,
        Duration::from_millis(10),
        credential(),
        wire.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "TimedOut");
    assert!(error.post_may_have_been_sent);
    assert_eq!(wire.observed.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn credential_echo_malformed_json_and_decoded_response_budget_are_not_output() {
    let c = command(&["current", "--coordinates", "0,0"]);
    for payload in [
        json!({"results":[{"dateTime":"2026-11-15T17:47:00-05:00","echo":BEARER}]}),
        json!({"results":[{"dateTime":"2026-11-15T17:47:00-05:00","echo":SAS}]}),
        json!({"results":[],"accountSasToken":"unexpected"}),
    ] {
        let error = run(&c, credential(), wire(response(200, payload)))
            .await
            .unwrap_err();
        assert_eq!(error.code, "CredentialInResponse");
    }
    let malformed = Ok(AsyncRawResponse::from_bytes(
        200.into(),
        Headers::new(),
        b"not-json".to_vec(),
    ));
    assert_eq!(
        run(&c, credential(), wire(malformed))
            .await
            .unwrap_err()
            .code,
        "InvalidResponse"
    );
    let huge = Ok(AsyncRawResponse::from_bytes(
        200.into(),
        Headers::new(),
        vec![b' '; fabric_sdk::maps::MAX_RESPONSE_BYTES + 1],
    ));
    assert_eq!(
        run(&c, credential(), wire(huge)).await.unwrap_err().code,
        "ResponseLimit"
    );
}

#[test]
fn result_exports_roundtrip_nested_null_offsets_units_and_provenance_not_summaries() {
    let c = command(&["current", "--coordinates", "0,0"]);
    let payload = synthetic_weather();
    let result =
        ProcessingResult::parse(&c.request, &serde_json::to_vec(&payload).unwrap()).unwrap();
    let table = export_table(&result, true).unwrap();
    let bytes = encode(&table, TabularFormat::Json).unwrap();
    let rows: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(rows[0]["result"], payload["results"][0]);
    assert_eq!(rows[0]["validation"], "offline-synthetic-fixture");
    let csv = encode(&table, TabularFormat::Csv).unwrap();
    assert!(String::from_utf8(csv).unwrap().contains("dateTime"));
    let parquet = encode(&table, TabularFormat::Parquet).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("synthetic.parquet");
    std::fs::write(&path, parquet).unwrap();
    let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        std::fs::File::open(path).unwrap(),
    )
    .unwrap();
    let mut reader = builder.build().unwrap();
    let batch = reader.next().unwrap().unwrap();
    assert_eq!(batch.num_rows(), 1);
    let nested = batch
        .column_by_name("result")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::StringArray>()
        .unwrap();
    let restored: Value = serde_json::from_str(nested.value(0)).unwrap();
    assert_eq!(restored, payload["results"][0]);
    assert_eq!(
        batch.schema().field_with_name("result").unwrap().metadata()
            [fabric_cli_output::tabular::JSON_LOGICAL_TYPE_KEY],
        "json"
    );
    let projected: Value =
        serde_json::from_slice(&render(&result, Some("results[0].nullable"), false, true).unwrap())
            .unwrap();
    assert!(projected["data"].is_null());
    assert_eq!(projected["complete"], true);
    assert_eq!(projected["resultCount"], 1);
}

#[tokio::test]
async fn throttling_retains_retry_after_and_issuer_refusals_stop_before_provider() {
    let c = command(&["current", "--coordinates", "0,0"]);
    let mut headers = Headers::new();
    headers.insert("retry-after", "17");
    let wire = wire(Ok(AsyncRawResponse::from_bytes(
        429.into(),
        headers,
        b"private error text".to_vec(),
    )));
    let error = run(&c, credential(), wire.clone()).await.unwrap_err();
    assert_eq!(error.retry_after_seconds, Some(17));
    assert_eq!(wire.observed.lock().unwrap().len(), 3);
    let wire = Arc::new(Wire {
        responses: Mutex::new(VecDeque::from([
            response(
                200,
                json!({"backendUrl":"https://synthetic.analysis.windows.net/"}),
            ),
            response(403, json!({"error":"private text"})),
        ])),
        observed: Mutex::new(Vec::new()),
        suspend_provider: false,
    });
    assert_eq!(
        run(&c, credential(), wire.clone()).await.unwrap_err().code,
        "AdmissionDenied"
    );
    assert_eq!(wire.observed.lock().unwrap().len(), 2);
}

#[test]
fn empty_results_keep_schema_and_credential_bearing_fixture_is_withheld() {
    let c = command(&["geocode", "--address", "synthetic"]);
    let result = execution::offline_fixture(
        &c.request,
        b"{\"type\":\"FeatureCollection\",\"features\":[]}",
    )
    .unwrap();
    let table = export_table(&result, true).unwrap();
    assert_eq!(table.row_count(), 0);
    assert!(!encode(&table, TabularFormat::Parquet).unwrap().is_empty());
    assert_eq!(execution::offline_fixture(&c.request,b"{\"type\":\"FeatureCollection\",\"features\":[],\"accountSasToken\":\"not-a-credential\"}").unwrap_err().code,"CredentialInResponse");
}

#[test]
fn csv_escaping_cannot_exceed_the_maps_encoded_output_limit() {
    let c = command(&["geocode", "--address", "synthetic"]);
    let candidate = json!({"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"confidence":"High"}});
    let payload = json!({"type":"FeatureCollection","metadata":"\"".repeat(300_000),"features":vec![candidate;12]});
    let bytes = serde_json::to_vec(&payload).unwrap();
    assert!(bytes.len() < fabric_sdk::maps::MAX_RESPONSE_BYTES);
    let result = ProcessingResult::parse(&c.request, &bytes).unwrap();
    let rows = result.export_rows().unwrap();
    assert!(serde_json::to_vec(&rows).unwrap().len() < fabric_sdk::maps::MAX_OUTPUT_BYTES);
    let unconstrained = fabric_cli_output::tabular::TabularTable::from_rows_with_json_columns(
        &rows,
        &["result", "context", "responseMetadata"],
    )
    .unwrap();
    assert!(
        encode(&unconstrained, TabularFormat::Csv).unwrap().len()
            > fabric_sdk::maps::MAX_OUTPUT_BYTES
    );
    let bounded = export_table(&result, true).unwrap();
    assert!(encode(&bounded, TabularFormat::Csv).is_err());
}

#[test]
fn expanding_queries_are_rejected_before_evaluation_and_safe_paths_keep_native_types() {
    let c = command(&["current", "--coordinates", "0,0"]);
    let payload = json!({"results":[{"dateTime":"2026-11-15T17:47:00-05:00","temperature":{"value":8.3},"large":"x".repeat(1024*1024)}]});
    let result =
        ProcessingResult::parse(&c.request, &serde_json::to_vec(&payload).unwrap()).unwrap();
    let expanding = std::iter::repeat_n("[@,@]", 20)
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        parse(
            ["current", "--coordinates", "0,0", "--query", &expanding]
                .into_iter()
                .map(OsString::from)
        )
        .is_err()
    );
    assert!(render(&result, Some(&expanding), false, true).is_err());
    for query in [
        "results[*].temperature.value",
        "results[0].temperature.value",
        "results[].temperature.value",
    ] {
        let output: Value =
            serde_json::from_slice(&render(&result, Some(query), false, true).unwrap()).unwrap();
        assert!(output["data"] == json!([8.3]) || output["data"] == json!(8.3));
    }
    for query in [
        "map(&@, results)",
        "{a: @, b: @}",
        "results[?temperature.value]",
        "results[0].[large,large]",
    ] {
        assert!(
            render(&result, Some(query), false, true).is_err(),
            "{query}"
        );
    }
}
