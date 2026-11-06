// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Search parser and data-only projection contracts run without provider setup.

#![deny(unnameable_test_items)]

use fabric_cli::search_command::{SearchDelivery, parse_search, sdk_request};
use fabric_sdk::search::SearchMode;
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

const WORKSPACE: &str = "/workspaces/11111111-2222-4333-8444-555555555555";
const TENANT: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn cursor_json(path: &str, token: &str) -> String {
    let scope = match path.strip_prefix("/workspaces/") {
        Some(workspace) => json!({"kind":"workspace","tenantId":TENANT,"workspaceId":workspace}),
        None => json!({"kind":"tenant","tenantId":TENANT}),
    };
    json!({"scope":scope,"token":token}).to_string()
}

#[test]
fn failed_and_unknown_receipts_without_data_preserve_the_original_evidence() {
    for status in ["failed", "unknown"] {
        let command =
            parse_search(["search", "/", "--text", "x", "--project", "length(@)"]).unwrap();
        let delivery = SearchDelivery::preflight(&command, &std::env::temp_dir()).unwrap();
        let envelope = json!({
            "status":status,
            "error":{"code":"SourceFailed","message":"Source is unavailable."},
            "meta":{"sources":[{"source":"relevance","httpStatus":403}]}
        });
        let mut output = Vec::new();
        delivery.deliver(envelope.clone(), &mut output).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), envelope);
    }
}

#[test]
fn defaults_are_bounded_catalog_without_query_rewriting() {
    let command = parse_search(["search", "/", "--text", "sales && \"monthly\""]).unwrap();
    assert_eq!(command.mode, SearchMode::Catalog);
    assert_eq!(command.text.as_deref(), Some("sales && \"monthly\""));
    assert_eq!(command.limit, 50);
    assert_eq!(command.max_bytes.get(), 1_048_576);
    assert_eq!(command.timeout, Duration::from_secs(30));
    assert!(command.output.is_none());
}

#[test]
fn native_workspace_ids_and_relevance_utf16_limits_are_preserved() {
    let command =
        parse_search(["search", WORKSPACE, "--mode", "hybrid", "--text", "sales"]).unwrap();
    let request = sdk_request(&command, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap();
    assert_eq!(
        request.scope().workspace_id().unwrap().as_str(),
        WORKSPACE.strip_prefix("/workspaces/").unwrap()
    );
    for (text, accepted) in [
        ("a".repeat(500), true),
        ("a".repeat(501), false),
        ("\u{1f600}".repeat(250), true),
        ("\u{1f600}".repeat(251), false),
    ] {
        assert_eq!(
            parse_search(["search", "/", "--mode", "relevance", "--text", &text]).is_ok(),
            accepted
        );
    }
}

#[test]
fn command_debug_never_logs_search_text_or_native_cursor() {
    let cursor = cursor_json("/", "private-opaque-cursor");
    let command = parse_search(["search", "/", "--cursor", &cursor]).unwrap();
    assert!(!format!("{command:?}").contains("private-opaque-cursor"));
    let command = parse_search(["search", "/", "--text", "private-customer-query"]).unwrap();
    assert!(!format!("{command:?}").contains("private-customer-query"));
}

#[test]
fn documented_boundaries_and_filter_only_search_remain_explicit() {
    for limit in ["1", "1000"] {
        assert!(parse_search(["search", "/", "--text", "x", "--limit", limit]).is_ok());
    }
    for mode in ["relevance", "hybrid"] {
        assert!(
            parse_search([
                "search", "/", "--text", "x", "--mode", mode, "--limit", "50"
            ])
            .is_ok()
        );
        assert!(
            parse_search([
                "search", "/", "--text", "x", "--mode", mode, "--limit", "51"
            ])
            .is_err()
        );
    }
    assert!(parse_search(["search", "/", "--type", "Lakehouse"]).is_ok());
    assert!(parse_search(["search", WORKSPACE]).is_ok());
    assert!(parse_search(["search", "/"]).is_err());
    for limit in ["0", "1001", "-1"] {
        assert!(parse_search(["search", "/", "--text", "x", "--limit", limit]).is_err());
    }
}

#[test]
fn native_resume_never_accepts_fresh_criteria_or_hybrid_scope() {
    let cursor = cursor_json("/", "opaque +/% token");
    let resumed = parse_search(["search", "/", "--cursor", &cursor]).unwrap();
    assert_eq!(resumed.cursor.as_ref().unwrap().token(), "opaque +/% token");
    for args in [
        vec!["search", "/", "--cursor", &cursor, "--text", "new"],
        vec!["search", "/", "--cursor", &cursor, "--type", "Report"],
        vec!["search", WORKSPACE, "--cursor", &cursor],
        vec!["search", "/", "--cursor", &cursor, "--mode", "hybrid"],
    ] {
        assert!(parse_search(args).is_err());
    }
}

#[test]
fn cursor_resume_preserves_original_workspace_and_rejects_another_tenant() {
    let cursor = cursor_json(WORKSPACE, "opaque +/% native token");
    let command = parse_search(["search", WORKSPACE, "--cursor", &cursor]).unwrap();
    let request = sdk_request(&command, TENANT).unwrap();
    assert_eq!(
        request.scope().workspace_id().unwrap().as_str(),
        WORKSPACE.strip_prefix("/workspaces/").unwrap()
    );
    assert!(parse_search(["search", "/", "--cursor", &cursor]).is_err());
    assert!(sdk_request(&command, "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff").is_err());
    assert!(parse_search(["search", "/", "--cursor", "not a native cursor JSON"]).is_err());
}

#[test]
fn invalid_or_private_arguments_are_not_echoed() {
    for args in [
        vec![
            "search",
            "/private.Workspace/secret.Notebook",
            "--text",
            "secret",
        ],
        vec!["search", "/", "--text", "secret", "--format", "yaml"],
        vec!["search", "/", "--text", "secret", "--timeout", "0s"],
        vec!["search", "/", "--text", "secret", "--timeout", "1.5s"],
        vec!["search", "/", "--text", "secret", "--timeout", "25h"],
        vec!["search", "/", "--text", "secret", "--project", "["],
        vec!["search", "/", "--text", "first", "--text", "secret"],
        vec!["search", "/", "--text", "secret", "--lim", "1"],
    ] {
        let error = parse_search(args).unwrap_err().to_string();
        assert!(!error.contains("secret"));
        assert!(!error.contains("private"));
    }
}

#[test]
fn scalar_projection_preserves_status_and_source_evidence() {
    let command = parse_search(["search", "/", "--text", "x", "--project", "length(@)"]).unwrap();
    let delivery = SearchDelivery::preflight(&command, &std::env::temp_dir()).unwrap();
    let meta =
        json!({"sources":[{"source":"catalog","status":"partial","continuationToken":"next"}]});
    let mut output = Vec::new();
    delivery
        .deliver(
            json!({"status":"partial","data":[{"id":"a"},{"id":"b"}],"meta":meta}),
            &mut output,
        )
        .unwrap();
    let envelope: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(envelope["data"], 2);
    assert_eq!(envelope["status"], "partial");
    assert_eq!(envelope["meta"], meta);
}

#[test]
fn existing_output_fails_preflight_and_raced_creation_does_not_overwrite() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "fabric-search-output-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("output.json");
    let command = parse_search([
        "search",
        "/",
        "--text",
        "x",
        "--output",
        file.to_str().unwrap(),
    ])
    .unwrap();
    let delivery = SearchDelivery::preflight(&command, &root).unwrap();
    std::fs::write(&file, b"preserve").unwrap();
    assert!(SearchDelivery::preflight(&command, &root).is_err());
    assert!(
        delivery
            .deliver(
                json!({"status":"succeeded","data":[],"meta":{}}),
                &mut Vec::new()
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"preserve");
    std::fs::remove_file(file).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn unprojected_bytes_and_writer_failure_are_not_successful_output() {
    let command = parse_search([
        "search",
        "/",
        "--text",
        "x",
        "--project",
        "`null`",
        "--max-bytes",
        "64",
    ])
    .unwrap();
    let delivery = SearchDelivery::preflight(&command, &std::env::temp_dir()).unwrap();
    assert!(
        delivery
            .deliver(
                json!({"status":"succeeded","data":"x".repeat(100),"meta":{}}),
                &mut Vec::new()
            )
            .is_err()
    );

    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic writer failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let command = parse_search(["search", "/", "--text", "x"]).unwrap();
    let delivery = SearchDelivery::preflight(&command, &std::env::temp_dir()).unwrap();
    assert!(
        delivery
            .deliver(
                json!({"status":"succeeded","data":[],"meta":{}}),
                &mut BrokenWriter
            )
            .is_err()
    );
}
