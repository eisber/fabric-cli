// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Strict export decode policy at the real root/HTTP/filesystem boundary.

use super::*;

#[tokio::test]
async fn noncanonical_percent_and_padding_fail_before_any_export_write_or_success_fallback() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../../fabric-deploy/tests/definition_export_base64_source.json"
    ))
    .unwrap();
    for case in receipt["cases"].as_array().unwrap().iter().filter(|case| {
        case["name"] != "canonical" && case["name"] != "source-error-original-payload"
    }) {
        let mut host = host();
        host.metadata_with_id(
            WS,
            json!([{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]),
        );
        host.reply(200, json!({"id":PIPELINE}));
        host.reply(200,json!({"definition":{"parts":[
            {"path":"first.txt","payload":"eA=="},
            {"path":"noncanonical.txt","payload":case["input"]["definition"]["parts"][0]["payload"]},
        ]}}));
        let output = host
            .execute(&[
                "export",
                "Demo.Workspace/Book.DataPipeline",
                "-f",
                "-o",
                "output",
            ])
            .await;
        failed(&output);
        let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
            unreachable!()
        };
        assert!(error.completed_paths.is_empty(), "{}", case["name"]);
        assert!(error.exported_ids.is_empty());
        let TransferFailure::Definition(error) = &error.failure else {
            unreachable!()
        };
        assert!(matches!(
            error.kind,
            fabric_deploy::definitions::DefinitionErrorKind::InvalidDefinition
        ));
        assert!(error.incomplete_path.is_none());
        assert!(
            fs::read_dir(host.root.join("output"))
                .unwrap()
                .next()
                .is_none(),
            "no decoded bytes or original encoded string may be written for {}",
            case["name"]
        );
        let requests = host.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "POST")
                .count(),
            1,
            "one definition read, no automatic replay"
        );
    }
}
