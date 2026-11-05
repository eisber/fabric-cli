// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Downstream trait-object executor injection without a transport or credential backend.

#![deny(unnameable_test_items)]

use fabric_sdk_dataagent_ontology::{
    inputs::ElementsOptions,
    paging::PagingLimits,
    private::{Limits, PrivateClient},
    result::Outcome,
    scope::{AgentScope, CallerScope, Guid, Stage},
};
use fabric_sdk_ontology_workload::workload::{
    WorkloadError, WorkloadExecutor, WorkloadFuture, WorkloadIdentity,
    WorkloadMethod, WorkloadRequest, WorkloadResponse,
};
use std::{
    collections::BTreeMap,
    future::pending,
    sync::{Arc, atomic::{AtomicUsize, Ordering}},
};

struct InjectedExecutor(AtomicUsize);

impl WorkloadExecutor for InjectedExecutor {
    fn execute<'a>(
        &'a self,
        request: &'a WorkloadRequest,
    ) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.method(), WorkloadMethod::Get);
            assert!(request.body().is_empty());
            assert!(!request.route().as_str().contains("/staging/"));
            Ok(WorkloadResponse {
                status: 200,
                headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
                body: br#"{"value":[]}"#.to_vec(),
            })
        })
    }
}

#[tokio::test]
async fn downstream_arc_trait_object_executes_single_read_and_shared_pager() {
    let concrete = Arc::new(InjectedExecutor(AtomicUsize::new(0)));
    let executor: Arc<dyn WorkloadExecutor> = concrete.clone();
    let id = |number| Guid::new(format!("11111111-2222-3333-4444-{number:012}")).unwrap();
    let client = PrivateClient::new(
        AgentScope { workspace_id: id(1), agent_id: id(2) },
        CallerScope::new("offline-account", "offline-tenant").unwrap(),
        WorkloadIdentity::new("caller-dataagent").unwrap(),
        Limits::default(),
    );
    let Outcome::Completed(result) = client.execute(
        executor.as_ref(), client.list(Stage::Published).unwrap(), pending(),
    ).await.unwrap() else { panic!("not completed") };
    assert!(result.value.value.is_empty());
    assert!(result.validator.is_none());
    let elements = client.collect_elements(
        executor.as_ref(),
        client.elements(Stage::Published, id(3), &ElementsOptions::default()).unwrap(),
        "https://caller-bound.invalid/".parse().unwrap(),
        PagingLimits::default(),
        pending(),
    ).await.unwrap();
    assert!(elements.value.is_empty());
    assert_eq!(elements.pages_received, 1);
    assert_eq!(concrete.0.load(Ordering::SeqCst), 2);
}
