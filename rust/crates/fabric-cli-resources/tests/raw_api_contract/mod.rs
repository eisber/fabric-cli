// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
mod bindings;
use azure_core::http::Method;
use fabric_cli_resources::{ApiInput, ApiOptions, api, build_api_request, read_api_input};
use fabric_sdk_api::{Origin, RequestBounds};

fn options(case: &Value) -> ApiOptions {
    ApiOptions {
        origin: match case["audience"].as_str().unwrap_or("fabric") {
            "fabric" => Origin::Fabric,
            "storage" => Origin::OneLake,
            "azure" => Origin::Arm,
            "powerbi" => Origin::PowerBi,
            _ => panic!("fixture origin"),
        },
        method: match case["method"].as_str().unwrap_or("get") {
            "get" => Method::Get,
            "post" => Method::Post,
            "put" => Method::Put,
            "patch" => Method::Patch,
            "delete" => Method::Delete,
            _ => panic!("fixture method"),
        },
        endpoint: case["endpoint"].as_str().unwrap_or("resources").into(),
        params: case["params"].as_str().map(str::to_owned),
        headers: case["headers"].as_str().map(str::to_owned),
        input: case
            .get("input")
            .filter(|v| !v.is_null())
            .map(|v| ApiInput::Json(v.clone())),
        query: case["query"].as_str().map(str::to_owned),
        show_headers: case["show_headers"].as_bool().unwrap_or(false),
        bounds: RequestBounds::default(),
    }
}

#[tokio::test]
async fn raw_fabric_sdk_http_and_output_match_independent_source_verbs_status_data_and_queries() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/raw_api/cases.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let options = options(case);
        let expected = &case["expected"];
        if !expected["error"].is_null() {
            let error = build_api_request(&options).unwrap_err();
            assert_eq!(error.code, expected["error"]["code"].as_str().unwrap());
            assert_eq!(
                error.message,
                expected["error"]["message"].as_str().unwrap()
            );
            continue;
        }
        let request = build_api_request(&options).unwrap();
        assert_eq!(
            request.url().as_str(),
            expected["requests"][0]["url"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            request.method().to_string(),
            expected["requests"][0]["method"].as_str().unwrap()
        );
        assert_eq!(
            request.origin().audience().scope(),
            expected["scopes"][0][0].as_str().unwrap()
        );
        let raw = case["raw_body"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| case["response"].to_string());
        let headers: Vec<_> = case["response_headers"]
            .as_object()
            .into_iter()
            .flat_map(|h| h.iter())
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
            .collect();
        let (client, wire, credential) = setup_audience(
            vec![reply(
                case["status"].as_u64().unwrap() as u16,
                &raw,
                &headers,
            )],
            options.origin.audience(),
        );
        let actual = api(
            &client,
            &options,
            NonZeroUsize::new(4096).unwrap(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(
            actual.data.as_ref().unwrap(),
            &expected["data"],
            "{}",
            case["name"]
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            actual
                .write_json(
                    "api",
                    Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                    &mut out,
                    &mut err
                )
                .unwrap(),
            expected["exit"].as_u64().unwrap() as u8
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            expected["stdout"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert!(err.is_empty());
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url().as_str(),
            expected["requests"][0]["url"].as_str().unwrap()
        );
        for (name, value) in request.headers() {
            assert_eq!(
                requests[0].headers().get_str(&name.clone().into()).unwrap(),
                value
            );
        }
        let bytes = azure_core::Bytes::from(requests[0].body());
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice::<Value>(&bytes).unwrap()
        };
        assert_eq!(body, expected["requests"][0]["body"]);
    }
}

#[test]
fn raw_input_and_multipart_use_real_bounded_files_without_missing_file_fallback() {
    let limit = NonZeroUsize::new(16).unwrap();
    assert!(matches!(
        read_api_input(&[r#"{"x":1}"#.into()], "boundary", limit).unwrap(),
        Some(ApiInput::Json(_))
    ));
    assert_eq!(
        read_api_input(&["missing-7196-input.json".into()], "boundary", limit)
            .unwrap_err()
            .code,
        "InvalidPath"
    );
    let path = std::env::temp_dir().join(format!("fabric-api-7196-{}.bin", std::process::id()));
    std::fs::write(&path, b"abc").unwrap();
    let input = read_api_input(&[path.to_str().unwrap().into()], "boundary", limit)
        .unwrap()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let mut args = options(&json!({}));
    args.input = Some(input);
    let request = build_api_request(&args).unwrap();
    assert!(
        request
            .headers()
            .iter()
            .any(|(k, v)| k == "content-type" && v == "multipart/form-data; boundary=boundary")
    );
    assert!(request.body().ends_with(b"abc\r\n--boundary--\r\n"));
    for name in ["Authorization", "HOST", "Proxy-Authorization"] {
        args.headers = Some(format!("{name}=not-allowed"));
        assert!(build_api_request(&args).is_err());
    }
}
