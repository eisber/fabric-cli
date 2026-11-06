// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public metadata argv -> ItemMutations -> shared OperationSession -> wire contracts.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "capability_support/mod.rs"]
mod support;

use fabric_cli_ontology::{
    CapabilityCause, CapabilityError, OntologyVerb as Verb, execute_metadata,
};
use fabric_sdk_ontology::v2::{Outcome, Precondition, TargetKind};
use fabric_sdk_operations::OperationExecutionError;
use serde_json::{Value, json};
use std::{fs, sync::atomic::Ordering, time::Duration};
use support::*;

#[tokio::test]
async fn real_collection_create_uses_no_fake_item_id_and_generic_facade_match() {
    let mut receipts = vec![];
    for facade in [false, true] {
        let args = arguments(Verb::Create, &["--input", "-"], facade);
        let mut host = Host::new(item_target(true), "item.metadata.create");
        host.body(json!({"displayName":"Knowledge","description":"authored"}));
        let wire = PublicWire::new(vec![public_response(201, metadata())], host.events.clone());
        let (client, credential) = public_client();
        let report = execute_metadata(
            &args,
            facade,
            &registry(),
            &mut host,
            &client,
            wire.clone(),
            public_limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        assert!(report.command_succeeded());
        assert_eq!(
            report.receipt.target().selection().kind,
            TargetKind::WorkspaceCollection
        );
        assert_eq!(report.receipt.target().identity().item_id(), None);
        assert_eq!(report.receipt.target().selection().generation, None);
        assert_eq!(report.receipt.preconditions(), [Precondition::CreateOnly]);
        let seen = wire.calls();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/workspaces/{WORKSPACE}/ontologies"))
        );
        assert_eq!(
            seen[0]["body"],
            json!({"displayName":"Knowledge","description":"authored"})
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        assert!(host.stderr.is_empty());
        receipts.push(serde_json::from_slice::<Value>(&host.stdout.lock().unwrap()).unwrap());
    }
    assert_eq!(receipts[0], receipts[1]);
}

#[tokio::test]
async fn metadata_patch_reads_file_preserves_omission_and_reserves_real_output_before_wire() {
    let directory = TempRoot::new();
    fs::write(directory.input(), br#"{"description":"changed"}"#).unwrap();
    let (input, output) = (directory.input(), directory.output());
    let args = arguments(
        Verb::Update,
        &[
            "--input",
            input.to_str().unwrap(),
            "--mode",
            "patch",
            "-o",
            output.to_str().unwrap(),
        ],
        true,
    );
    let mut host = Host::new(item_target(false), "item.metadata.patch");
    let wire = PublicWire::new(vec![public_response(200, metadata())], host.events.clone());
    let (client, _) = public_client();
    let report = execute_metadata(
        &args,
        true,
        &registry(),
        &mut host,
        &client,
        wire.clone(),
        public_limits(),
        tokio::time::sleep(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    assert!(report.command_succeeded());
    assert!(host.stdout.lock().unwrap().is_empty());
    assert!(host.stderr.is_empty());
    assert_eq!(wire.calls()[0]["method"], "Patch");
    assert_eq!(wire.calls()[0]["body"], json!({"description":"changed"}));
    assert!(
        report
            .receipt
            .preconditions()
            .contains(&Precondition::UnguardedConcurrency)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(output).unwrap()).unwrap()["status"],
        "succeeded"
    );
}

#[tokio::test]
async fn soft_and_permanent_delete_use_generic_items_route_without_reset_or_fallback() {
    for permanent in [false, true] {
        let args = arguments(
            Verb::Delete,
            if permanent { &["--permanent"] } else { &[] },
            false,
        );
        let mut host = Host::new(item_target(false), "item.delete");
        let wire = PublicWire::new(vec![public_response(200, Value::Null)], host.events.clone());
        let (client, _) = public_client();
        let report = execute_metadata(
            &args,
            false,
            &registry(),
            &mut host,
            &client,
            wire.clone(),
            public_limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        assert!(report.command_succeeded());
        let seen = wire.calls();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["method"], "Delete");
        assert!(
            seen[0]["url"]
                .as_str()
                .unwrap()
                .contains(&format!("/workspaces/{WORKSPACE}/items/{ITEM}"))
        );
        assert_eq!(
            seen[0]["url"].as_str().unwrap().contains("hardDelete=true"),
            permanent
        );
        assert_eq!(
            report
                .receipt
                .preconditions()
                .contains(&Precondition::PermanentDeletion),
            permanent
        );
    }
}

#[tokio::test]
async fn input_schema_cardinality_revision_approval_and_no_clobber_refuse_before_credentials() {
    for (verb, collection, body, options, refuse, reserve) in [
        (
            Verb::Create,
            false,
            json!({"displayName":"Knowledge"}),
            vec!["--input", "-"],
            false,
            false,
        ),
        (
            Verb::Create,
            true,
            json!({"displayName":"Knowledge","type":"Ontology"}),
            vec!["--input", "-"],
            false,
            false,
        ),
        (
            Verb::Create,
            true,
            json!({"displayName":"Knowledge","definition":{}}),
            vec!["--input", "-"],
            false,
            false,
        ),
        (
            Verb::Update,
            false,
            json!({"description":null}),
            vec!["--input", "-", "--mode", "patch"],
            false,
            false,
        ),
        (
            Verb::Update,
            false,
            json!({"generation":2}),
            vec!["--input", "-", "--mode", "patch"],
            false,
            false,
        ),
        (
            Verb::Update,
            false,
            json!({"description":"safe"}),
            vec![
                "--input",
                "-",
                "--mode",
                "patch",
                "--if-match",
                "unsupported",
            ],
            false,
            false,
        ),
        (
            Verb::Update,
            false,
            json!({"description":"safe"}),
            vec!["--input", "-", "--mode", "patch"],
            true,
            false,
        ),
        (
            Verb::Update,
            false,
            json!({"description":"safe"}),
            vec!["--input", "-", "--mode", "patch"],
            false,
            true,
        ),
    ] {
        let id = if verb == Verb::Create {
            "item.metadata.create"
        } else {
            "item.metadata.patch"
        };
        let args = arguments(verb, &options, false);
        let mut host = Host::new(item_target(collection), id);
        host.body(body);
        host.refuse = refuse;
        host.fail_reservation = reserve;
        let wire = PublicWire::new(vec![], host.events.clone());
        let (client, credential) = public_client();
        assert!(
            execute_metadata(
                &args,
                false,
                &registry(),
                &mut host,
                &client,
                wire.clone(),
                public_limits(),
                tokio::time::sleep(Duration::from_secs(5))
            )
            .await
            .is_err()
        );
        assert!(wire.calls().is_empty());
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
        assert!(host.stdout.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn accepted_metadata_is_submission_and_poll_http_failure_remains_unknown() {
    for wait in [false, true] {
        let options = if wait {
            vec!["--input", "-", "--mode", "patch", "--wait"]
        } else {
            vec!["--input", "-", "--mode", "patch"]
        };
        let args = arguments(Verb::Update, &options, false);
        let mut host = Host::new(item_target(false), "item.metadata.patch");
        host.body(json!({"description":"changed"}));
        let mut replies = vec![accepted()];
        if wait {
            replies.push(public_response(
                403,
                json!({"error":{"code":"Denied","message":"private-provider-text"}}),
            ));
        }
        let wire = PublicWire::new(replies, host.events.clone());
        let (client, _) = public_client();
        let report = execute_metadata(
            &args,
            false,
            &registry(),
            &mut host,
            &client,
            wire.clone(),
            public_limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        assert_eq!(report.receipt.evidence().handles.len(), 1);
        assert!(report.receipt.evidence().mutation_may_have_occurred);
        if wait {
            assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
            assert!(matches!(report.cause, Some(CapabilityCause::Metadata(_))));
            assert!(host.stdout.lock().unwrap().is_empty());
            assert!(!String::from_utf8_lossy(&host.stderr).contains("private-provider-text"));
            assert_eq!(wire.calls().len(), 2);
        } else {
            assert!(matches!(report.receipt.outcome(), Outcome::Accepted));
            assert!(host.stderr.is_empty());
            assert_eq!(wire.calls().len(), 1);
        }
    }
}

#[tokio::test]
async fn uncertain_transport_and_writer_failure_keep_original_evidence_without_replay() {
    for writer_failure in [false, true] {
        let args = arguments(Verb::Update, &["--input", "-", "--mode", "patch"], false);
        let mut host = Host::new(item_target(false), "item.metadata.patch");
        host.body(json!({"description":"changed"}));
        host.fail_writer = writer_failure;
        let wire = PublicWire::new(vec![], host.events.clone());
        wire.replies.lock().unwrap().push_back(if writer_failure {
            Ok(public_response(200, metadata()))
        } else {
            Err(OperationExecutionError::Failed)
        });
        let (client, _) = public_client();
        let result = execute_metadata(
            &args,
            false,
            &registry(),
            &mut host,
            &client,
            wire.clone(),
            public_limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await;
        if writer_failure {
            let Err(CapabilityError::Output { report, .. }) = result else {
                panic!("original execution report required");
            };
            assert!(matches!(
                report.receipt.outcome(),
                Outcome::Succeeded { .. }
            ));
            assert_eq!(host.stdout.lock().unwrap().len(), 7);
        } else {
            let report = result.unwrap();
            assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
            assert!(report.receipt.evidence().mutation_may_have_occurred);
        }
        assert_eq!(wire.calls().len(), 1);
    }
}

#[tokio::test]
async fn provider_terminal_cancellation_is_failed_but_local_observation_cancellation_is_unknown() {
    for provider_terminal in [false, true] {
        let args = arguments(
            Verb::Update,
            &["--input", "-", "--mode", "patch", "--wait"],
            false,
        );
        let mut host = Host::new(item_target(false), "item.metadata.patch");
        host.body(json!({"description": "changed"}));
        let wire = PublicWire::new(vec![accepted()], host.events.clone());
        wire.replies
            .lock()
            .unwrap()
            .push_back(if provider_terminal {
                Ok(public_response(200, json!({"status": "Cancelled"})))
            } else {
                Err(OperationExecutionError::Cancelled)
            });
        let (client, _) = public_client();
        let report = execute_metadata(
            &args,
            false,
            &registry(),
            &mut host,
            &client,
            wire.clone(),
            public_limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        if provider_terminal {
            assert!(matches!(report.receipt.outcome(), Outcome::Failed { .. }));
        } else {
            assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
        }
        let Some(CapabilityCause::Metadata(error)) = report.cause else {
            panic!("original SDK cancellation evidence required");
        };
        assert_eq!(error.code, "LongRunningOperationCancelled");
        assert_eq!(error.raw_service_error.is_some(), provider_terminal);
        assert_eq!(report.receipt.evidence().handles.len(), 1);
        assert!(report.receipt.evidence().mutation_may_have_occurred);
        assert!(host.stdout.lock().unwrap().is_empty());
        assert_eq!(wire.calls().len(), 2);
    }
}

#[tokio::test]
async fn malformed_created_identity_is_not_success_and_cancelled_mutation_does_not_dispatch() {
    let args = arguments(Verb::Create, &["--input", "-"], false);
    let mut host = Host::new(item_target(true), "item.metadata.create");
    host.body(json!({"displayName":"Knowledge"}));
    let wire = PublicWire::new(
        vec![public_response(201, json!({"id":"not-an-item"}))],
        host.events.clone(),
    );
    let (client, credential) = public_client();
    let report = execute_metadata(
        &args,
        false,
        &registry(),
        &mut host,
        &client,
        wire.clone(),
        public_limits(),
        tokio::time::sleep(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert_eq!(report.unpresented_data, Some(json!({"id":"not-an-item"})));
    assert!(host.stdout.lock().unwrap().is_empty());
    assert_eq!(wire.calls().len(), 1);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);

    let mut host = Host::new(item_target(true), "item.metadata.create");
    host.body(json!({"displayName":"Knowledge"}));
    let wire = PublicWire::new(vec![], host.events.clone());
    let report = execute_metadata(
        &args,
        false,
        &registry(),
        &mut host,
        &client,
        wire.clone(),
        public_limits(),
        async {},
    )
    .await
    .unwrap();
    assert!(!report.command_succeeded());
    assert!(!report.receipt.evidence().mutation_may_have_occurred);
    assert!(wire.calls().is_empty());
}

#[tokio::test]
async fn create_preserves_unknown_generation_but_rejects_malformed_generation_without_defaulting() {
    for generation in [Value::Null, json!("2")] {
        let mut returned = metadata();
        returned["properties"]["generation"] = generation.clone();
        let mut host = Host::new(item_target(true), "item.metadata.create");
        host.body(json!({"displayName":"Knowledge"}));
        let wire = PublicWire::new(
            vec![public_response(201, returned.clone())],
            host.events.clone(),
        );
        let (client, _) = public_client();
        let report = execute_metadata(
            &arguments(Verb::Create, &["--input", "-"], false),
            false,
            &registry(),
            &mut host,
            &client,
            wire.clone(),
            public_limits(),
            tokio::time::sleep(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        if generation.is_null() {
            assert!(
                matches!(report.receipt.outcome(),Outcome::Succeeded { data } if data==&returned)
            );
            assert_eq!(report.receipt.target().selection().generation, None);
        } else {
            assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
            assert!(report.unpresented_data.is_some());
            assert!(host.stdout.lock().unwrap().is_empty());
        }
        assert_eq!(wire.calls().len(), 1);
    }
}
