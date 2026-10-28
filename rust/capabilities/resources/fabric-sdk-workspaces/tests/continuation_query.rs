// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public opt-in continuation attachment; ordinary resource operations stay stateless.

use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOperation};
use serde_json::json;

fn operation(method: ResourceMethod) -> ResourceOperation {
    ResourceOperation::new(
        method,
        &["admin", "domains", "domain"],
        Some(json!({"displayName":"Name"})),
        CompletionPolicy::PythonSynchronous,
    )
    .unwrap()
}

#[test]
fn opaque_token_is_encoded_once_and_cannot_change_route_body_method_or_completion() {
    let request = operation(ResourceMethod::Patch)
        .with_continuation_token(" +%2A*/?#=&~日")
        .unwrap();
    assert_eq!(
        request.path(),
        "/v1/admin/domains/domain?continuationToken=+%2B%252A%2A%2F%3F%23%3D%26~%E6%97%A5"
    );
    assert_eq!(request.method(), ResourceMethod::Patch);
    assert_eq!(request.body(), Some(&json!({"displayName":"Name"})));
    assert_eq!(request.completion(), CompletionPolicy::PythonSynchronous);
    assert!(!format!("{request:?}").contains("continuationToken"));
    assert_eq!(
        operation(ResourceMethod::Patch).path(),
        "/v1/admin/domains/domain"
    );
}

#[test]
fn explicit_attachment_replaces_token_and_collapses_query_keys_at_first_insertion_position() {
    let request = operation(ResourceMethod::Post)
        .with_query(&[
            ("before", "first"),
            ("continuationToken", "old"),
            ("before", "last"),
            ("other", "*~+"),
            ("continuationToken", "older"),
            ("ContinuationToken", "case-sensitive"),
        ])
        .unwrap()
        .with_continuation_token("new&token")
        .unwrap()
        .with_continuation_token("final")
        .unwrap();
    assert_eq!(
        request.path(),
        "/v1/admin/domains/domain?before=last&continuationToken=final&other=%2A~%2B&ContinuationToken=case-sensitive"
    );
}

#[test]
fn default_query_builder_empty_token_and_hard_delete_defaults_remain_unchanged() {
    let ordinary = operation(ResourceMethod::Patch)
        .with_query(&[("value", "*~")])
        .unwrap();
    assert_eq!(ordinary.path(), "/v1/admin/domains/domain?value=*%7E");
    assert_eq!(
        ordinary.with_continuation_token("").unwrap().path(),
        "/v1/admin/domains/domain?value=%2A~"
    );
    assert_eq!(
        operation(ResourceMethod::Patch)
            .with_continuation_token("")
            .unwrap()
            .path(),
        "/v1/admin/domains/domain"
    );
    assert_eq!(
        operation(ResourceMethod::Delete)
            .with_hard_delete()
            .unwrap()
            .path(),
        "/v1/admin/domains/domain?hardDelete=true"
    );
    assert!(operation(ResourceMethod::Patch).with_hard_delete().is_err());
    let hard_first = operation(ResourceMethod::Delete)
        .with_hard_delete()
        .unwrap()
        .with_continuation_token("*~")
        .unwrap();
    let token_first = operation(ResourceMethod::Delete)
        .with_continuation_token("*~")
        .unwrap()
        .with_hard_delete()
        .unwrap();
    assert_eq!(
        hard_first.path(),
        "/v1/admin/domains/domain?hardDelete=true&continuationToken=%2A~"
    );
    assert_eq!(
        token_first.path(),
        "/v1/admin/domains/domain?continuationToken=%2A~&hardDelete=true"
    );
}

#[test]
fn invalid_tokens_fail_before_dispatch_without_loosening_constructor_boundaries() {
    for token in [
        "line\nbreak",
        "carriage\rreturn",
        "\0",
        "\t",
        "\u{7f}",
        "\u{85}",
    ] {
        let error = operation(ResourceMethod::Patch)
            .with_continuation_token(token)
            .unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(!error.mutation_may_have_occurred);
        assert!(error.operation_id.is_none());
    }
    for segment in [
        "",
        "..",
        "a?continuationToken=unsafe",
        "a/b",
        "https://other.invalid",
        "a%20b",
    ] {
        assert!(
            ResourceOperation::new(
                ResourceMethod::Post,
                &["admin", segment],
                None,
                CompletionPolicy::ReturnAccepted
            )
            .is_err()
        );
    }
    assert!(
        ResourceOperation::new(
            ResourceMethod::Get,
            &["admin", "domains"],
            Some(json!({})),
            CompletionPolicy::ReturnAccepted
        )
        .is_err()
    );
    assert!(
        ResourceOperation::new(
            ResourceMethod::Post,
            &["admin", "domains"],
            Some(json!([])),
            CompletionPolicy::ReturnAccepted
        )
        .is_err()
    );
}
