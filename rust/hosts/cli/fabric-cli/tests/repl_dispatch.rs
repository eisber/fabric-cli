// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Existing plan-only consumers and additive typed-result consumers agree offline.

#![deny(unnameable_test_items)]

#[allow(dead_code, unused_imports)]
mod folder_support;

use fabric_cli::{
    ReadConfig,
    jobs::JobReadSession,
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_jobs::HttpJobExecutor;
use fabric_cli_output::Timestamp;
use folder_support::{Credential, Http, Timer, complete, executor, sdk};
use std::{future::ready, sync::Arc};

#[test]
fn read_result_retains_typed_cancellation_and_the_existing_render_plan() {
    let Invocation::Read(command) = parse_args(["fab-rs", "get", "Demo.Workspace"]).unwrap() else {
        panic!("read required");
    };
    let http = Arc::new(Http::new(Vec::new()));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let source = session.with_source_operations(&executor);
    let output = complete(source.dispatch_result(&command, ready(()))).unwrap();
    let envelope = output
        .error
        .as_ref()
        .unwrap()
        .envelope(session.timestamp.clone(), None);
    assert_eq!(
        envelope.result().error_code(),
        Some("LongRunningOperationCancelled")
    );
    let previous = complete(source.dispatch(&command, ready(()))).unwrap();
    assert_eq!(output.plan, previous);
    assert!(credential.scopes.lock().unwrap().is_empty());
    http.assert_consumed();
}

#[test]
fn job_result_retains_typed_cancellation_and_the_existing_render_plan() {
    let Invocation::JobRead(command) =
        parse_args(["fab-rs", "job", "run-list", "Demo.Workspace/Book.Notebook"]).unwrap()
    else {
        panic!("job read required");
    };
    let http = Arc::new(Http::new(Vec::new()));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::new(Credential::default()));
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let session = JobReadSession {
        reads: &reads,
        jobs: Arc::new(HttpJobExecutor::new(executor.clone())),
    };
    let output = complete(session.dispatch_result(&command, ready(()))).unwrap();
    let envelope = output
        .error
        .as_ref()
        .unwrap()
        .envelope(reads.timestamp.clone(), None);
    assert_eq!(
        envelope.result().error_code(),
        Some("LongRunningOperationCancelled")
    );
    let previous = complete(session.dispatch(&command, ready(()))).unwrap();
    assert_eq!(output.plan, previous);
    http.assert_consumed();
}
