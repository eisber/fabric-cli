// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Strict, non-disclosing custom header parsing.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use fabric_cli_api::parse_headers;

#[test]
fn json_headers_preserve_values_and_normalize_only_names() {
    let headers = parse_headers(Some(
        r#"{"Accept":"application/json","Content-Type":" text/plain ","If-Match":"a=b,c","If-None-Match":"*","Prefer":"return=minimal","Range":"bytes=0-9"}"#,
    ))
    .unwrap();
    assert_eq!(headers.len(), 6);
    assert_eq!(headers["content-type"], " text/plain ");
    assert_eq!(headers["if-match"], "a=b,c");
    assert!(parse_headers(None).unwrap().is_empty());
    assert!(parse_headers(Some("{}")).unwrap().is_empty());
}

#[test]
fn invalid_shapes_duplicates_protected_fields_and_injection_fail_without_values() {
    for text in [
        "",
        "null",
        "[]",
        "true",
        "1",
        r#""CANARY""#,
        "Accept=CANARY",
        r#"{"Accept":null}"#,
        r#"{"Accept":1}"#,
        r#"{"Accept":true}"#,
        r#"{"Accept":[]}"#,
        r#"{"Accept":{"secret":"CANARY"}}"#,
        r#"{"Accept":"CANARY","accept":"other"}"#,
        r#"{"Accept":"CANARY","Accept":"other"}"#,
        r#"{"Acce\u0070t":"CANARY","Accept":"other"}"#,
        r#"{"$serde_json::private::Number":"CANARY"}"#,
        r#"{"Authorization":"CANARY"}"#,
        r#"{"PrOxY-aUtHoRiZaTiOn":"CANARY"}"#,
        r#"{"Cookie":"CANARY"}"#,
        r#"{"Host":"CANARY"}"#,
        r#"{"Content-Length":"1"}"#,
        r#"{"Connection":"CANARY"}"#,
        r#"{"x-ms-version":"CANARY"}"#,
        r#"{"User-Agent":"CANARY"}"#,
        r#"{"x-ms-client-request-id":"CANARY"}"#,
        r#"{"x-ms-fabric-skill":"CANARY"}"#,
        r#"{"Accept":"CANARY\r\nInjected: true"}"#,
        r#"{"Accept":"CANARY\u0000"}"#,
        r#"{"Accept":"CANARY"} trailing"#,
        r#"'{"Accept":"CANARY"}'"#,
    ] {
        let error = parse_headers(Some(text)).unwrap_err();
        assert!(!format!("{error:?}").contains("CANARY"), "{text}");
    }
    let oversized = format!(r#"{{"Accept":"{}"}}"#, "x".repeat(64 * 1024));
    assert!(parse_headers(Some(&oversized)).is_err());
}
