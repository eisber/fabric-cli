// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned SJD destination rejection and genuine local fallback at the root boundary.

use super::*;
use fabric_cli::families::transfer_files::TokioTransferFiles;

#[tokio::test]
async fn pinned_sjd_destination_decisions_do_not_invent_remote_resources_support() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../../../../capabilities/deployment/fabric-deploy/tests/definition_export_destination_source.json"
    ))
    .unwrap();
    for case in receipt["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["type"] == "SparkJobDefinition")
    {
        let mut host = host();
        let input = case["input"].as_str().unwrap();
        if case["local_exists"] == true {
            fs::create_dir_all(host.root.join(input)).unwrap();
        }
        host.metadata_with_id(
            WS,
            json!([{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]),
        );
        host.reply(200, json!({"id":PIPELINE}));
        host.reply(200, definition("part.txt", "eA=="));
        host.metadata_with_id(
            WS,
            json!([{"id":LAKE,"displayName":"Job","type":"SparkJobDefinition"}]),
        );
        let mut files = TokioTransferFiles::defer_export_root(
            host.root.join(input),
            input.into(),
            Default::default(),
            tokio::runtime::Handle::current(),
        );
        let http = host.http.clone();
        let output = injected_export_with_http(
            &mut host,
            &mut files,
            pending(),
            &[
                "fab-rs",
                "export",
                "Demo.Workspace/Book.DataPipeline",
                "-f",
                "-o",
                input,
            ],
            http,
        )
        .await;
        if case["error"].is_null() {
            success(&output);
            assert_eq!(case["destination"]["type"], "local");
            assert_eq!(
                fs::read(
                    host.root
                        .join(input)
                        .join("Book.DataPipeline")
                        .join("part.txt")
                )
                .unwrap(),
                b"x"
            );
        } else {
            failed(&output);
            let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
                unreachable!()
            };
            let envelope = error.envelope(
                fabric_cli_output::Timestamp::parse("2026-10-28T14:50:00Z").unwrap(),
                None,
            );
            assert_eq!(
                envelope.result().error_code(),
                case["error"]["code"].as_str()
            );
            if case["error"]["code"] == "NotSupported" {
                assert_eq!(
                    envelope.result().message(),
                    case["error"]["message"].as_str()
                );
            }
        }
        let requests = host.requests();
        assert_eq!(requests.len(), 6);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "POST")
                .count(),
            1,
            "only source getDefinition POST, never a storage write"
        );
        assert!(requests.iter().all(|request| {
            request["url"]
                .as_str()
                .unwrap()
                .starts_with("https://api.fabric.microsoft.com/")
        }));
        assert!(
            host.credential
                .0
                .lock()
                .unwrap()
                .iter()
                .all(|scopes| scopes == &[fabric_sdk::Audience::Fabric.scope()])
        );
        assert_eq!(case["remote_write_calls"], json!([]));
    }
}
