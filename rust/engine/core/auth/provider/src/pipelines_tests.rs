// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure Azure Pipelines source, endpoint and signed-header contract tests.
#![deny(unnameable_test_items)]

use super::*;

const ID: &str = "11111111-1111-1111-1111-111111111111";

fn endpoint() -> String {
    format!(
        "https://dev.azure.com/organization/{ID}/_apis/distributedtask/hubs/build/plans/{ID}/jobs/{ID}/oidctoken"
    )
}

fn input(name: &str) -> Option<OsString> {
    Some(match name {
        "SYSTEM_COLLECTIONURI" => "https://dev.azure.com/organization/".into(),
        "SYSTEM_TEAMPROJECTID" | "SYSTEM_PLANID" | "SYSTEM_JOBID" => ID.into(),
        "SYSTEM_HOSTTYPE" => "build".into(),
        "SYSTEM_OIDCREQUESTURI" => endpoint().into(),
        _ => panic!("unselected input read"),
    })
}

fn config() -> PipelinesConfig {
    PipelinesConfig::from_inputs(ID, &endpoint(), Secret::new("synthetic-token"), input).unwrap()
}

#[test]
fn capture_rejects_endpoint_substitution_and_request_escaping_without_environment_mutation() {
    let expected = endpoint();
    for endpoint in [
        expected.replace("dev.azure.com", "evil.example"),
        expected.replace("/organization/", "/different/"),
        expected.replace("/build/", "/rm/"),
        expected.replace("/jobs/", "/%6aobs/"),
        expected.replace("https://", "http://"),
        expected.replace("dev.azure.com", "user@dev.azure.com"),
        expected.replace("dev.azure.com", "dev.azure.com:444"),
        format!("{expected}?api-version=7.1"),
        format!("{expected}?serviceConnectionId={ID}"),
        format!("{expected}#fragment"),
        format!("{expected}/"),
    ] {
        assert!(
            PipelinesConfig::from_inputs(ID, &endpoint, Secret::new("synthetic-token"), |name| {
                if name == "SYSTEM_OIDCREQUESTURI" {
                    Some(endpoint.clone().into())
                } else {
                    input(name)
                }
            },)
            .is_err()
        );
    }
    for connection in [
        "",
        "not-a-guid",
        "../escape",
        "id&serviceConnectionId=other",
    ] {
        assert!(
            PipelinesConfig::from_inputs(
                connection,
                &expected,
                Secret::new("synthetic-token"),
                input,
            )
            .is_err()
        );
    }
    for token in ["", "contains space", "line\r\ninjection"] {
        assert!(PipelinesConfig::from_inputs(ID, &expected, Secret::new(token), input).is_err());
    }
    for name in RUN_VARIABLES {
        assert!(
            PipelinesConfig::from_inputs(ID, &expected, Secret::new("synthetic-token"), |key| {
                if key == name { None } else { input(key) }
            })
            .is_err()
        );
    }
}

#[test]
fn every_running_pipeline_source_is_revalidated_and_secrets_are_not_bindings() {
    let selected = config();
    assert!(selected.validate_with(input).is_ok());
    for changed in RUN_VARIABLES {
        assert!(
            selected
                .validate_with(|name| {
                    if name == changed {
                        Some("drift".into())
                    } else {
                        input(name)
                    }
                })
                .is_err()
        );
        assert!(
            selected
                .validate_with(|name| { if name == changed { None } else { input(name) } })
                .is_err()
        );
    }
    assert!(!format!("{selected:?} {}", selected.binding()).contains("synthetic-token"));
    let rotated =
        PipelinesConfig::from_inputs(ID, &endpoint(), Secret::new("rotated-token"), input).unwrap();
    assert_eq!(selected.binding(), rotated.binding());
}

#[derive(Debug)]
struct Never;

#[async_trait]
impl HttpClient for Never {
    async fn execute_request(&self, _: &Request) -> azure_core::Result<AsyncRawResponse> {
        panic!("unexpected HTTP");
    }
}

fn guard() -> PipelinesHttp {
    PipelinesHttp {
        token_http: CredentialHttp {
            inner: Arc::new(Never),
            endpoint: Url::parse(&format!(
                "https://login.microsoftonline.com/{ID}/oauth2/v2.0/token"
            ))
            .unwrap(),
            host: None,
            managed_client: None,
        },
        config: config(),
    }
}

fn oidc_request(guard: &PipelinesHttp) -> Request {
    let mut request = Request::new(guard.config.request_url.clone(), Method::Post);
    request.insert_header("authorization", "Bearer synthetic-token");
    request.insert_header(FEDAUTH, "Suppress");
    request.insert_header("content-length", "0");
    request
}

#[test]
fn request_guard_rejects_wrong_signed_header_redirect_target_method_and_query() {
    let guard = guard();
    assert!(guard.validate_request(&oidc_request(&guard)).unwrap());
    for (name, value) in [
        ("authorization", "Bearer different-token"),
        (FEDAUTH, "Follow"),
        ("content-length", "1"),
    ] {
        let mut request = oidc_request(&guard);
        request.insert_header(name, value);
        assert!(guard.validate_request(&request).is_err());
    }
    for url in [
        guard
            .config
            .request_url
            .as_str()
            .replace("organization", "other"),
        guard
            .config
            .request_url
            .as_str()
            .replace("dev.azure.com", "evil.example"),
        format!("{}&serviceConnectionId={ID}", guard.config.request_url),
        format!("{}#fragment", guard.config.request_url),
    ] {
        let request = Request::new(Url::parse(&url).unwrap(), Method::Post);
        assert!(guard.validate_request(&request).is_err());
    }
    let get = Request::new(guard.config.request_url.clone(), Method::Get);
    assert!(guard.validate_request(&get).is_err());
    let mut body = oidc_request(&guard);
    body.set_body("must-be-empty");
    assert!(guard.validate_request(&body).is_err());
    let mut token = Request::new(guard.token_http.endpoint.clone(), Method::Post);
    assert!(!guard.validate_request(&token).unwrap());
    token.insert_header("authorization", "Bearer synthetic-token");
    assert!(guard.validate_request(&token).is_err());
    let mut token = Request::new(guard.token_http.endpoint.clone(), Method::Post);
    token.insert_header(FEDAUTH, "Suppress");
    assert!(guard.validate_request(&token).is_err());
}

#[tokio::test]
async fn source_drift_prevents_both_authenticated_oidc_and_token_exchange_sends() {
    let guard = guard();
    for request in [
        oidc_request(&guard),
        Request::new(guard.token_http.endpoint.clone(), Method::Post),
    ] {
        for changed in RUN_VARIABLES {
            let error = guard
                .execute_with(&request, |name| {
                    if name == changed {
                        Some("drift".into())
                    } else {
                        input(name)
                    }
                })
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                HeadlessError::PipelineContract.to_string()
            );
        }
    }
}
