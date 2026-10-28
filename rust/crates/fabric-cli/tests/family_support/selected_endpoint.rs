// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    CREATED, WORKSPACE,
    family_support::{Harness, success},
};
use fabric_cli::families::FamilyEvidence;
use serde_json::{Value, json};

const OPERATION: &str = "44444444-4444-4444-4444-444444444444";
const HOSTS: [&str; 2] = [
    "dailyapi.fabric.microsoft.com",
    "msitapi.fabric.microsoft.com",
];

fn input(host: &Harness) {
    let input = host.root.join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("raw.bin"), [0_u8, 255, 13, 10]).unwrap();
}

fn assert_binding(host: &Harness, hostname: &str) {
    let prefix = format!("https://{hostname}/v1/");
    assert!(
        host.requests()
            .iter()
            .all(|request| request["url"].as_str().unwrap().starts_with(&prefix))
    );
    let scopes = host.credential.0.lock().unwrap();
    assert!(!scopes.is_empty());
    assert!(
        scopes
            .iter()
            .all(|scopes| scopes == &[fabric_sdk::Audience::Fabric.scope()])
    );
}

#[tokio::test]
async fn selected_import_endpoint_binds_discovery_mutation_poll_and_result() {
    for hostname in HOSTS {
        let mut host = Harness::new().with_endpoint(hostname);
        input(&host);
        host.metadata_with_id(WORKSPACE, json!([]));
        host.headers(202, json!({}), &[("x-ms-operation-id", OPERATION)]);
        let result_url = format!("https://{hostname}/v1/operations/{OPERATION}/result");
        host.headers(
            200,
            json!({"status":"Succeeded"}),
            &[("Location", &result_url)],
        );
        host.reply(200, json!({"id":CREATED}));

        let output = host
            .execute(&[
                "import",
                "Demo.Workspace/Synthetic.DataPipeline",
                "-i",
                "input",
                "-f",
            ])
            .await;

        success(&output);
        assert_binding(&host, hostname);
        let requests = host.requests();
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests
                .iter()
                .map(|request| request["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["GET", "GET", "POST", "GET", "GET"]
        );
        assert!(
            requests[3]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/operations/{OPERATION}"))
        );
        assert!(
            requests[4]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/operations/{OPERATION}/result"))
        );
        assert!(
            matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
                if result.effects[0].item_id == CREATED)
        );
    }
}

#[tokio::test]
async fn selected_environment_endpoint_is_retained_by_the_second_publication_session() {
    for hostname in HOSTS {
        let mut host = Harness::new().with_endpoint(hostname);
        input(&host);
        host.metadata_with_id(WORKSPACE, json!([]));
        host.reply(201, json!({"id":CREATED}));
        host.reply(200, Value::Null);
        host.reply(
            200,
            json!({"properties":{"publishDetails":{"state":"success"}}}),
        );

        let output = host
            .execute(&[
                "import",
                "Demo.Workspace/Synthetic.Environment",
                "-i",
                "input",
                "-f",
            ])
            .await;

        success(&output);
        assert_binding(&host, hostname);
        let requests = host.requests();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[3]["method"], "POST");
        assert!(
            requests[3]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/environments/{CREATED}/staging/publish"))
        );
        assert_eq!(requests[4]["method"], "GET");
        assert!(
            matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
                if result.effects[0].item_id == CREATED)
        );
    }
}
