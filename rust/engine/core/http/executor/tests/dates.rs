// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Independent HTTP-date and overflow expectations.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use fabric_core::ApiResponse;
use fabric_sdk_http::retry_after;
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

#[test]
fn retry_after_accepts_all_http_date_forms_and_huge_delta_without_early_retry() {
    for date in [
        "Sun, 06 Nov 1994 08:49:37 GMT",
        "Sunday, 06-Nov-94 08:49:37 GMT",
        "Sun Nov  6 08:49:37 1994",
    ] {
        let response = ApiResponse {
            status: 202,
            headers: BTreeMap::from([("retry-after".into(), date.into())]),
            body: vec![],
        };
        assert_eq!(
            retry_after(&response, SystemTime::UNIX_EPOCH).unwrap(),
            Some(Duration::from_secs(784111777))
        );
    }
    let response = ApiResponse {
        status: 202,
        headers: BTreeMap::from([("retry-after".into(), "99999999999999999999999999".into())]),
        body: vec![],
    };
    assert_eq!(
        retry_after(&response, SystemTime::UNIX_EPOCH).unwrap(),
        Some(Duration::MAX)
    );
}
