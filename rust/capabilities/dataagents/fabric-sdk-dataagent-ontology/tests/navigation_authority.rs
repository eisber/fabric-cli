// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Configuration-file and navigation-representation validator authority contracts.

#![deny(unnameable_test_items)]

use fabric_sdk_dataagent_ontology::{
    ValidationError,
    inputs::SelectEntities,
    private::{Limits, PrivateClient},
    result::Outcome,
    scope::{
        AgentScope, AttachmentScope, CallerScope, Guid, Resource, SourceScope, Stage, Validator,
    },
};
use fabric_sdk_ontology_workload::workload::{
    WorkloadError, WorkloadExecutor, WorkloadFuture, WorkloadIdentity, WorkloadRequest,
    WorkloadResponse,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    future::pending,
    sync::Mutex,
};

fn id(number: u8) -> Guid {
    Guid::new(format!("11111111-2222-3333-4444-{number:012}")).unwrap()
}

fn agent() -> AgentScope {
    AgentScope {
        workspace_id: id(1),
        agent_id: id(2),
    }
}

fn caller() -> CallerScope {
    CallerScope::new("offline-account", "offline-tenant").unwrap()
}

struct Executor {
    responses: Mutex<VecDeque<WorkloadResponse>>,
    conditions: Mutex<Vec<Option<String>>>,
}

impl WorkloadExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: &'a WorkloadRequest,
    ) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>> {
        Box::pin(async move {
            self.conditions
                .lock()
                .unwrap()
                .push(request.if_match().map(str::to_owned));
            Ok(self.responses.lock().unwrap().pop_front().unwrap())
        })
    }
}

#[tokio::test]
async fn configuration_file_etag_from_attachment_read_still_authorizes_native_selection() {
    let response = |body| WorkloadResponse {
        status: 200,
        headers: BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("etag".into(), "\"configuration-file-etag\"".into()),
        ]),
        body: serde_json::to_vec(&body).unwrap(),
    };
    let executor = Executor {
        responses: Mutex::new(VecDeque::from([
            response(json!({"id": id(3)})),
            response(json!({"elementsStatus": []})),
        ])),
        conditions: Mutex::new(Vec::new()),
    };
    let client = PrivateClient::new(
        agent(),
        caller(),
        WorkloadIdentity::new("caller-dataagent").unwrap(),
        Limits::default(),
    );
    let Outcome::Completed(metadata) = client
        .execute(
            &executor,
            client.read(Stage::Staging, id(3)).unwrap(),
            pending(),
        )
        .await
        .unwrap()
    else {
        panic!("not completed")
    };
    let tag = metadata.validator.unwrap();
    let operation = client
        .select(id(3), &SelectEntities::all(false), Some(&tag))
        .unwrap();
    client
        .execute(&executor, operation, pending())
        .await
        .unwrap();
    assert_eq!(
        *executor.conditions.lock().unwrap(),
        [None, Some("\"configuration-file-etag\"".into())],
    );
}

#[test]
fn cached_schema_source_navigation_and_authored_setup_are_distinct_validator_scopes() {
    let source = SourceScope {
        attachment: AttachmentScope {
            agent: agent(),
            stage: Stage::Staging,
            ontology_id: id(3),
        },
        source_id: id(4),
    };
    let schema = Resource::SourceSchema(source.clone());
    let tag = Validator::new(caller(), schema.clone(), "\"cached-schema-etag\"").unwrap();
    assert_eq!(
        tag.for_resource(&caller(), &schema).unwrap(),
        "\"cached-schema-etag\""
    );
    for different in [
        Resource::SourceElements(source.clone()),
        Resource::Instructions(source.clone()),
        Resource::CustomSetup(source),
    ] {
        assert_eq!(
            tag.for_resource(&caller(), &different),
            Err(ValidationError::ValidatorScope)
        );
    }
}
