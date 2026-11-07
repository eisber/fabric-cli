// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Search request and schema consumers use the SDK facade without a CLI host.

#![deny(unnameable_test_items)]

use fabric_sdk::search::{
    InitialSearch, SearchLimits, SearchMode, SearchRequest, SearchScope, TypeFilters, schemas,
};
use serde_json::json;

const TENANT: &str = "11111111-2222-4333-8444-555555555555";

#[test]
fn facade_request_round_trip_uses_the_single_validated_contract() {
    let request = SearchRequest::Initial(
        InitialSearch::new(
            SearchMode::Catalog,
            SearchScope::tenant(TENANT).unwrap(),
            Some("sales".into()),
            TypeFilters::new(vec!["Report".into()], vec![]).unwrap(),
            SearchLimits::new(50, 50, 1_048_576, 1).unwrap(),
        )
        .unwrap(),
    );
    let wire = serde_json::to_value(&request).unwrap();
    let decoded = SearchRequest::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
    assert_eq!(request.scope().tenant_id().as_str(), TENANT);
}

#[test]
fn facade_schema_is_local_and_unknown_input_fields_are_not_ignored() {
    let schema = schemas();
    assert_eq!(
        schema.input["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert!(schema.output.is_object());
    assert!(
        SearchRequest::from_value(json!({
            "kind":"initial",
            "scope":{"kind":"tenant","tenantId":TENANT},
            "query":"sales",
            "backend":"invented"
        }))
        .is_err()
    );
}
