// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Production root binary with only identity and service transports injected.
#![deny(unnameable_test_items)]

use super::*;
use serde_json::{Value, json};

const TOKEN: &str =
    r#"{"access_token":"synthetic-fabric-token","token_type":"Bearer","expires_in":3600}"#;
const WORKSPACES: &str = r#"{"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}"#;
const ITEMS: &str = r#"{"value":[{"id":"n","displayName":"marc","type":"Notebook"},{"id":"r","displayName":"marcia","type":"Report"},{"id":"x","displayName":"Marc","type":"Notebook"}]}"#;

async fn invoke(
    args: &[&str],
    current: &str,
    replies: impl IntoIterator<Item = &'static str>,
) -> (u8, Value, Vec<Request>) {
    let http = Arc::new(Wire {
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::new(Vec::new()),
    });
    let invocation =
        fabric_cli::parser::parse_args(std::iter::once("fab-rs").chain(args.iter().copied()))
            .unwrap();
    let command = match &invocation {
        Invocation::Read(read) => NativeCommand::Read(read),
        Invocation::Resource(_) => NativeCommand::Family(&invocation),
        _ => panic!("read or resource invocation"),
    };
    let credential = NativeCredential::Headless(selection("client-secret", None));
    let mut state = BatchState::capture();
    credential
        .bind(&mut state, fabric_cli_context::HomeContext::Unspecified)
        .unwrap();
    let (plan, outcome) = execute_injected(
        command,
        (credential, http.clone(), http.clone()),
        current,
        fabric_cli_context::HomeContext::Unspecified,
        EndpointBinding::public(ServiceOrigin::Fabric),
        settings::NativeSettings::select(None).unwrap(),
        &mut state,
    )
    .await
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert!(stderr.is_empty());
    let value: Value = serde_json::from_slice(&stdout).unwrap();
    let CommandOutcome::Returned(exit) = outcome else {
        panic!("ordinary command outcome");
    };
    assert!(http.replies.lock().unwrap().is_empty());
    let requests = http.requests.lock().unwrap().clone();
    assert!(requests.iter().all(|request| {
        request.url().host_str() == Some("login.microsoftonline.com")
            || (request.method() == Method::Get
                && request.url().host_str() == Some("api.fabric.microsoft.com")
                && !request.url().as_str().contains('*')
                && !request.url().as_str().contains("%2A"))
    }));
    (exit, value, requests)
}

#[tokio::test]
async fn root_binary_prefix_consumes_complete_listing_without_workspace_fanout() {
    let (exit, output, requests) = invoke(
        &["ls", "marc*", "--output_format", "json"],
        "/",
        [
            TOKEN,
            r#"{"value":[{"id":"x","displayName":"Other","type":"Workspace"}],"continuationToken":"next"}"#,
            r#"{"value":[{"id":"m","displayName":"marc","type":"Workspace"},{"id":"p","displayName":"marcus","type":"Personal"},{"id":"u","displayName":"Marc","type":"Workspace"}]}"#,
        ],
    )
    .await;
    assert_eq!(exit, 0, "{output}");
    assert_eq!(
        output["result"]["data"],
        json!([{"name":"marc.Workspace"},{"name":"marcus.Personal"}])
    );
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].url().as_str(),
        "https://api.fabric.microsoft.com/v1/workspaces?continuationToken=next"
    );
}

#[tokio::test]
async fn root_binary_exact_prefix_suffix_and_no_match_preserve_read_routing() {
    for (path, expected) in [
        (
            "/Demo.Workspace",
            json!([{"name":"marc.Notebook"},{"name":"Marc.Notebook"},{"name":"marcia.Report"}]),
        ),
        (
            "/Demo.Workspace/marc*",
            json!([{"name":"marc.Notebook"},{"name":"marcia.Report"}]),
        ),
        (
            "/Demo.Workspace/marc.notebook*",
            json!([{"name":"marc.Notebook"}]),
        ),
        (
            "/Demo.Workspace/marc.noteboo\u{212a}*",
            json!([{"name":"marc.Notebook"}]),
        ),
        ("/Demo.Workspace/missing*", json!([])),
    ] {
        let (exit, output, requests) = invoke(
            &["ls", path, "--output_format", "json"],
            "/",
            [TOKEN, WORKSPACES, ITEMS],
        )
        .await;
        assert_eq!(exit, 0, "{path}: {output}");
        assert_eq!(output["result"]["data"], expected, "{path}");
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[2].url().as_str(),
            "https://api.fabric.microsoft.com/v1/workspaces/w/items"
        );
    }
}

#[tokio::test]
async fn root_binary_exact_punctuation_remains_literal() {
    for (path, workspaces) in [
        (
            "/Demo?.Workspace",
            r#"{"value":[{"id":"w","displayName":"Demo?","type":"Workspace"}]}"#,
        ),
        (
            "/Demo[.Workspace",
            r#"{"value":[{"id":"w","displayName":"Demo[","type":"Workspace"}]}"#,
        ),
        (
            "/Demo].Workspace",
            r#"{"value":[{"id":"w","displayName":"Demo]","type":"Workspace"}]}"#,
        ),
    ] {
        let (exit, output, requests) = invoke(
            &["ls", path, "--output_format", "json"],
            "/",
            [TOKEN, workspaces, ITEMS],
        )
        .await;
        assert_eq!(exit, 0, "{path}: {output}");
        assert_eq!(output["result"]["data"].as_array().unwrap().len(), 3);
        assert_eq!(requests.len(), 3);
    }
}

#[tokio::test]
async fn root_binary_rejects_malformed_prefixes_without_token_or_metadata_requests() {
    for path in ["*", "mar**", "mar?*", "mar[ab]*", "/mar*/child", "/mar*/"] {
        let (exit, output, requests) =
            invoke(&["ls", path, "--output_format", "json"], "/", []).await;
        assert_eq!(exit, 1, "{path}: {output}");
        assert_eq!(output["result"]["error_code"], "InvalidPath");
        assert!(requests.is_empty());
    }
}

#[tokio::test]
async fn root_binary_mutations_do_not_acquire_readonly_prefix_expansion() {
    let root = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "native_read::headless_host_tests::prefix::mutation_child",
            "--nocapture",
        ])
        .env_clear()
        .env("FAB_PREFIX_MUTATION_TEST", "1")
        .env("FAB_RUST_CONTEXT", "/")
        .env("FAB_RUST_CONFIG_ROOT", root.path().join("config"))
        .current_dir(root.path());
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            child.env(name, value);
        }
    }
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("prefix-mutations-checked"));
}

#[tokio::test]
async fn mutation_child() {
    if std::env::var("FAB_PREFIX_MUTATION_TEST").ok().as_deref() != Some("1") {
        return;
    }
    for (path, replies) in [
        ("marc*", vec![]),
        ("/Demo.Workspace/marc*", vec![TOKEN, WORKSPACES]),
        (
            "/Demo.Workspace/marc*.Notebook",
            vec![TOKEN, WORKSPACES, ITEMS],
        ),
    ] {
        let (exit, output, requests) = invoke(
            &["rm", path, "--force", "--output_format", "json"],
            "/",
            replies,
        )
        .await;
        assert_ne!(exit, 0, "{path}: {output}");
        assert!(output["result"].get("data").is_none());
        assert!(requests.iter().all(|request| {
            request.url().host_str() == Some("login.microsoftonline.com")
                || request.method() == Method::Get
        }));
    }
    println!("prefix-mutations-checked");
}
