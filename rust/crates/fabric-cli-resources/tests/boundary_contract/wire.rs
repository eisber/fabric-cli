// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use azure_core::http::Method;
use fabric_sdk_api::Request as ApiRequest;

#[tokio::test]
async fn actual_shared_http_request_bytes_match_python_ascii_spaces_order_and_numeric_wire_contracts()
 {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/access/wire_cases.json"
    ))
    .unwrap();
    for case in fixture["requests"].as_array().unwrap() {
        let (api, wire, _) = setup(vec![reply(200, "{}", &[])]);
        let request = ApiRequest::fabric(Method::Post, &["synthetic"])
            .unwrap()
            .with_json(&case["body"])
            .unwrap();
        api.send(&request, NonZeroUsize::new(4096).unwrap(), pending())
            .await
            .unwrap();
        let requests = wire.requests.lock().unwrap();
        let bytes = azure_core::Bytes::from(requests[0].body());
        assert_eq!(
            bytes.as_ref(),
            case["wire"].as_str().unwrap().as_bytes(),
            "{}",
            case["name"]
        );
        assert_eq!(
            fabric_sdk_api::json_body(&case["body"], NonZeroUsize::new(bytes.len()).unwrap())
                .unwrap(),
            bytes
        );
        assert_eq!(
            fabric_sdk_api::json_body(&case["body"], NonZeroUsize::new(bytes.len() - 1).unwrap())
                .unwrap_err(),
            ApiError::Limit
        );
    }
}

#[tokio::test]
async fn actual_shared_paged_acl_source_text_matches_python_substring_semantics_without_reencoding_response_bytes()
 {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/access/wire_cases.json"
    ))
    .unwrap();
    let case = &fixture["merged"];
    let (api, wire, _) = setup(vec![
        reply(200, case["first"].as_str().unwrap(), &[]),
        reply(200, case["second"].as_str().unwrap(), &[]),
    ]);
    let result = AccessClient::new(api)
        .list(&resource("workspace"), Limits::default(), pending())
        .await
        .unwrap();
    assert_eq!(result.source_text, case["wire"].as_str().unwrap());
    for identity in case["identities"].as_array().unwrap() {
        assert_eq!(
            result
                .source_text
                .contains(identity["value"].as_str().unwrap()),
            identity["matched"].as_bool().unwrap()
        );
    }
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
}
