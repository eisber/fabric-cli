// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Nonfatal operation failures remain visible and retain original workflow evidence.

use super::*;
use fabric_cli::deploy::DeployFailure;
use fabric_cli_transfer::{TransferEvent, TransferFuture, TransferUi};
use std::sync::Mutex;

#[derive(Default)]
struct Ui {
    events: Mutex<Vec<TransferEvent>>,
    fail: bool,
}

impl TransferUi for Ui {
    fn emit(&self, event: TransferEvent) -> Result<(), fabric_cli_output::CommandError> {
        self.events.lock().unwrap().push(event);
        if self.fail {
            Err(fabric_cli_output::CommandError::new(
                Some("Synthetic warning delivery failure".into()),
                Some("OutputFailed".into()),
            ))
        } else {
            Ok(())
        }
    }

    fn confirm<'a>(
        &'a self,
        _: &'a str,
    ) -> TransferFuture<'a, Result<bool, fabric_cli_output::CommandError>> {
        panic!("forced deployment must not prompt")
    }

    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, fabric_cli_output::CommandError>> {
        panic!("deployment must not select exports")
    }
}

#[tokio::test]
async fn failed_orphan_deletion_is_reported_and_retained_without_mutation_replay() {
    for delivery_fails in [false, true] {
        let mut host = Harness::new();
        input(&host, false);
        std::fs::write(
            host.root.join("deploy.yml"),
            format!("core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [CopyJob]\npublish:\n  skip: true\nfeatures: [disable_workspace_folder_publish]\n"),
        )
        .unwrap();
        let inventory = json!({"value":[{"id":ITEM,"displayName":"Orphan","type":"CopyJob"}]});
        host.reply(200, inventory.clone());
        host.reply(
            403,
            json!({"errorCode":"Denied","message":"Synthetic denial"}),
        );
        host.reply(200, inventory);
        host.reply(200, json!({"value":[]}));
        let ui = Ui {
            fail: delivery_fails,
            ..Default::default()
        };
        let output = host
            .execute_with_capabilities(
                &["deploy", "--config", "deploy.yml", "-f"],
                std::future::pending(),
                &ui,
            )
            .await;
        if delivery_fails {
            assert_ne!(output.plan.exit_code, 0);
            let fabric_cli::families::FamilyEvidence::Deploy(Err(error)) = &output.evidence else {
                panic!("warning delivery must fail explicitly")
            };
            let DeployFailure::Output { partial, .. } = &error.failure else {
                panic!("original workflow evidence must survive delivery failure")
            };
            assert_eq!(partial.warnings.len(), 1);
        } else {
            success(&output);
            let fabric_cli::families::FamilyEvidence::Deploy(Ok(result)) = &output.evidence else {
                panic!("source nonfatal deletion must retain its success-with-warning result")
            };
            let workflow = result.workflow.as_ref().unwrap();
            assert_eq!(workflow.warnings.len(), 1);
            assert!(matches!(
                workflow.warnings[0].kind,
                fabric_sdk_operations::OperationErrorKind::Http { status: 403, .. }
            ));
        }
        let events = ui.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], TransferEvent::Warning(message) if message.contains("forbidden"))
        );
        let requests = host.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "DELETE")
                .count(),
            1
        );
    }
}
