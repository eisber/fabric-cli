// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{
        self, LinkApproval, LinkEffects, LinkFiles, LinkOwner, LinkResolver, LinkScope,
        ResolvedLinkTarget,
    },
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::{Value, json};
use std::{future::Future, num::NonZeroUsize, pin::Pin};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const SOURCE: &str = "33333333-3333-4333-8333-333333333333";
const CONNECTION: &str = "44444444-4444-4444-8444-444444444444";

fn target() -> ResolvedLinkTarget {
    ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::Lakehouse,
        LinkScope::LakehouseShortcut {
            path: "Files/landing".into(),
            name: "sales_ref".into(),
        },
    )
    .unwrap()
}

fn input() -> Value {
    json!({
        "path": "Files/landing",
        "name": "sales_ref",
        "target": {
            "oneLake": {
                "workspaceId": WORKSPACE,
                "itemId": SOURCE,
                "path": "Files/sales"
            }
        }
    })
}

struct Operations {
    requests: Vec<ResourceOperation>,
    reply: Option<Result<ResourceOutcome, ResourceError>>,
}

impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        request: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.requests.push(request);
        Box::pin(std::future::ready(
            self.reply
                .take()
                .expect("a mutation must never be replayed"),
        ))
    }
}

struct Approval {
    approved: Result<bool, MutationError>,
    effects: Vec<LinkEffects>,
}

impl LinkApproval for Approval {
    fn approve<'a>(
        &'a mut self,
        _: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        self.effects.push(effects);
        Box::pin(std::future::ready(self.approved.clone()))
    }
}

fn approved() -> Approval {
    Approval {
        approved: Ok(true),
        effects: Vec::new(),
    }
}

fn operations(reply: Result<ResourceOutcome, ResourceError>) -> Operations {
    Operations {
        requests: Vec::new(),
        reply: Some(reply),
    }
}

#[test]
fn standalone_schema_is_consistent() {
    v2_link::command().debug_assert();
}

#[test]
fn common_and_facade_arguments_preserve_one_operation_schema() {
    let common = v2_link::parse_args([
        "fab-cli",
        "link",
        "/demo",
        "--input",
        "native.json",
        "--wait",
    ])
    .unwrap();
    for (facade, owner) in [
        ("data-agent", LinkOwner::DataAgent),
        ("kql-database", LinkOwner::KqlDatabase),
        ("lakehouse", LinkOwner::Lakehouse),
        ("ontology", LinkOwner::Ontology),
        ("semantic-model", LinkOwner::SemanticModel),
    ] {
        let scoped = v2_link::parse_args([
            "fab-cli",
            "artifact",
            facade,
            "link",
            "/demo",
            "--input",
            "native.json",
            "--wait",
        ])
        .unwrap();
        assert_eq!(common.path, scoped.path);
        assert_eq!(common.input, scoped.input);
        assert_eq!(common.wait, scoped.wait);
        assert_eq!(scoped.facade, Some(owner));
    }
}

#[test]
fn root_and_facade_prepare_identical_native_requests() {
    let common = v2_link::prepare(target(), None, &input(), false).unwrap();
    let facade = v2_link::prepare(target(), Some(LinkOwner::Lakehouse), &input(), false).unwrap();
    assert_eq!(common.operation_id(), facade.operation_id());
    assert_eq!(common.operation().method(), facade.operation().method());
    assert_eq!(common.operation().path(), facade.operation().path());
    assert_eq!(common.operation().body(), facade.operation().body());
    assert_eq!(common.effects(), facade.effects());
    assert_eq!(
        common.operation().completion(),
        CompletionPolicy::ReturnAccepted
    );
    assert!(
        common
            .operation()
            .path()
            .ends_with("shortcutConflictPolicy=Abort")
    );
}

#[test]
fn invalid_arguments_do_not_echo_input_or_enable_force() {
    for args in [
        vec![
            "fab-cli",
            "link",
            "/private",
            "--input",
            "secret.json",
            "--force",
        ],
        vec!["fab-cli", "link", "/private"],
        vec!["fab-cli", "link", "/private", "--input", ""],
        vec![
            "fab-cli",
            "link",
            "/private",
            "--input",
            "native.json",
            "extra",
        ],
    ] {
        let error = v2_link::parse_args(args).unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(!error.to_string().contains("/private"));
        assert!(!error.to_string().contains("secret.json"));
    }
}

#[test]
fn body_selectors_must_equal_the_resolved_destination() {
    for (key, value) in [("path", "Files/other"), ("name", "other")] {
        let mut body = input();
        body[key] = value.into();
        let error = v2_link::prepare(target(), None, &body, false).unwrap_err();
        assert_eq!(error.code, "InvalidInput");
    }
}

#[test]
fn wrong_facade_fails_instead_of_selecting_another_backend() {
    let error =
        v2_link::prepare(target(), Some(LinkOwner::DataAgent), &input(), false).unwrap_err();
    assert_eq!(error.code, "UnsupportedCapability");
}

#[test]
fn incompatible_owner_and_child_fail_locally() {
    let error = ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::SemanticModel,
        LinkScope::DataAgentStagingDatasources,
    )
    .unwrap_err();
    assert_eq!(error.code, "UnsupportedCapability");
}

#[test]
fn ontology_does_not_replace_the_definition_as_a_fallback() {
    let target = ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::Ontology,
        LinkScope::OntologySchema,
    )
    .unwrap();
    let error = v2_link::prepare(target, None, &json!({"definition":{}}), false).unwrap_err();
    assert_eq!(error.code, "UnsupportedCapability");
}

#[test]
fn kql_has_native_boolean_and_no_destination_path() {
    let target = ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::KqlDatabase,
        LinkScope::KqlTableShortcut {
            name: "sales".into(),
        },
    )
    .unwrap();
    let body = json!({
        "name":"sales",
        "enableQueryAcceleration":true,
        "target":{"oneLake":{"workspaceId":WORKSPACE,"itemId":SOURCE,"path":"Tables/sales"}}
    });
    let prepared = v2_link::prepare(target, None, &body, true).unwrap();
    assert!(prepared.effects().query_acceleration);
    assert!(!prepared.effects().update_item_sensitivity);
    assert_eq!(prepared.operation().body(), Some(&body));
    assert_eq!(
        prepared.operation().completion(),
        CompletionPolicy::WaitForResult
    );
    assert!(prepared.operation().path().contains("/kqlDatabases/"));
    assert!(
        !prepared
            .operation()
            .path()
            .contains("shortcutConflictPolicy")
    );
}

#[test]
fn sharepoint_sensitivity_is_an_explicit_approval_effect() {
    let body = json!({
        "path":"Files/landing",
        "name":"sales_ref",
        "target":{"oneDriveSharePoint":{
            "connectionId":CONNECTION,
            "location":"https://synthetic.sharepoint.com",
            "subpath":"/Shared Documents/data",
            "updateFabricItemSensitivity":true
        }}
    });
    let prepared = v2_link::prepare(target(), None, &body, false).unwrap();
    assert!(prepared.effects().update_item_sensitivity);
}

#[test]
fn dataagent_uses_staging_and_an_existing_by_id_reference() {
    let target = ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::DataAgent,
        LinkScope::DataAgentStagingDatasources,
    )
    .unwrap();
    let body = json!({
        "type":"LakehouseTables",
        "lakehouseReference":{"referenceType":"ById","workspaceId":WORKSPACE,"itemId":SOURCE}
    });
    let prepared = v2_link::prepare(target, None, &body, false).unwrap();
    assert!(
        prepared
            .operation()
            .path()
            .ends_with("/staging/datasources")
    );
    assert_eq!(prepared.operation().body(), Some(&body));
}

fn model() -> v2_link::PreparedLink {
    let target = ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::SemanticModel,
        LinkScope::SemanticModelConnections,
    )
    .unwrap();
    let body = json!({
        "connectionBinding":{
            "id":CONNECTION,
            "connectivityType":"ShareableCloud",
            "connectionDetails":{"type":"SQL","path":"synthetic.database.windows.net;sales"}
        }
    });
    v2_link::prepare(target, None, &body, true).unwrap()
}

#[test]
fn native_model_binding_uses_completion_without_inventing_a_result() {
    let prepared = model();
    assert_eq!(
        prepared.operation().completion(),
        CompletionPolicy::WaitForCompletion
    );
    assert!(prepared.operation().path().ends_with("/bindConnection"));
}

#[tokio::test]
async fn missing_or_declined_approval_never_dispatches() {
    let mut backend = operations(Ok(ResourceOutcome::Completed {
        status: 201,
        data: Some(json!({"name":"sales_ref"})),
    }));
    let error = v2_link::execute(
        v2_link::prepare(target(), None, &input(), false).unwrap(),
        &mut backend,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "ApprovalRequired");
    let mut approval = Approval {
        approved: Ok(false),
        effects: Vec::new(),
    };
    let error = v2_link::execute(
        v2_link::prepare(target(), None, &input(), false).unwrap(),
        &mut backend,
        Some(&mut approval),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "ApprovalRequired");
    assert!(backend.requests.is_empty());
}

#[tokio::test]
async fn failed_approval_is_an_error_not_an_implicit_decline() {
    let mut backend = operations(Ok(ResourceOutcome::Completed {
        status: 201,
        data: Some(json!({"name":"sales_ref"})),
    }));
    let mut approval = Approval {
        approved: Err(MutationError::new(
            "Cancelled",
            "Host approval was cancelled",
        )),
        effects: Vec::new(),
    };
    let error = v2_link::execute(
        v2_link::prepare(target(), None, &input(), false).unwrap(),
        &mut backend,
        Some(&mut approval),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "Cancelled");
    assert!(backend.requests.is_empty());
}

#[tokio::test]
async fn successful_create_keeps_actual_response_but_receipt_does_not_echo_input() {
    let outcome = ResourceOutcome::Completed {
        status: 201,
        data: Some(json!({"name":"sales_ref","path":"Files/landing"})),
    };
    let mut backend = operations(Ok(outcome.clone()));
    let mut approval = approved();
    let receipt = v2_link::execute(
        v2_link::prepare(target(), None, &input(), false).unwrap(),
        &mut backend,
        Some(&mut approval),
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcome, outcome);
    assert_eq!(backend.requests.len(), 1);
    assert_eq!(approval.effects, [receipt.effects]);
    let metadata = receipt.metadata();
    assert_eq!(metadata["status"], "succeeded");
    assert_eq!(metadata["target"]["itemId"], ITEM);
    assert!(metadata.get("target").unwrap().get("oneLake").is_none());
    assert!(!metadata.to_string().contains("Files/sales"));
}

#[tokio::test]
async fn native_bind_empty_200_is_completed_without_fabricated_resource() {
    let mut backend = operations(Ok(ResourceOutcome::Completed {
        status: 200,
        data: None,
    }));
    let mut approval = approved();
    let receipt = v2_link::execute(model(), &mut backend, Some(&mut approval))
        .await
        .unwrap();
    assert_eq!(receipt.metadata()["status"], "succeeded");
    assert!(matches!(
        receipt.outcome,
        ResourceOutcome::Completed { data: None, .. }
    ));
}

#[tokio::test]
async fn accepted_is_never_relabelled_completed_even_without_a_handle() {
    for operation_id in [None, Some("operation-1".to_owned())] {
        let outcome = ResourceOutcome::Accepted { operation_id };
        let mut backend = operations(Ok(outcome.clone()));
        let mut approval = approved();
        let receipt = v2_link::execute(
            v2_link::prepare(target(), None, &input(), false).unwrap(),
            &mut backend,
            Some(&mut approval),
        )
        .await
        .unwrap();
        assert_eq!(receipt.outcome, outcome);
        assert_eq!(receipt.metadata()["status"], "accepted");
        assert_eq!(backend.requests.len(), 1);
    }
}

#[tokio::test]
async fn uncertain_native_error_preserves_operation_and_is_not_replayed() {
    let mut native = ResourceError::input();
    native.code = "OutcomeUnknown".into();
    native.operation_id = Some("operation-1".into());
    native.mutation_may_have_occurred = true;
    native.raw_service_error = Some(Box::new(json!({"partial":{"name":"sales_ref"}})));
    let mut backend = operations(Err(native.clone()));
    let mut approval = approved();
    let error = v2_link::execute(
        v2_link::prepare(target(), None, &input(), true).unwrap(),
        &mut backend,
        Some(&mut approval),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.operation.as_deref(), Some(&native));
    assert_eq!(backend.requests.len(), 1);
    assert!(error.observed.is_none());
}

#[tokio::test]
async fn malformed_success_preserves_observation_and_possible_mutation() {
    let outcome = ResourceOutcome::Completed {
        status: 201,
        data: Some(json!({"unexpected":"actual"})),
    };
    let mut backend = operations(Ok(outcome.clone()));
    let mut approval = approved();
    let failure = v2_link::execute(
        v2_link::prepare(target(), None, &input(), false).unwrap(),
        &mut backend,
        Some(&mut approval),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.observed, Some(outcome));
    assert_eq!(failure.error.code, "InvalidResponse");
    assert!(
        failure
            .error
            .operation
            .as_ref()
            .unwrap()
            .mutation_may_have_occurred
    );
    assert_eq!(backend.requests.len(), 1);
}

#[tokio::test]
async fn completed_response_for_a_different_name_is_not_the_requested_link() {
    let outcome = ResourceOutcome::Completed {
        status: 201,
        data: Some(json!({"name":"unexpected_generated_name"})),
    };
    let mut backend = operations(Ok(outcome.clone()));
    let mut approval = approved();
    let failure = v2_link::execute(
        v2_link::prepare(target(), None, &input(), false).unwrap(),
        &mut backend,
        Some(&mut approval),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.observed, Some(outcome));
    assert_eq!(failure.error.code, "InvalidResponse");
    assert_eq!(backend.requests.len(), 1);
}

#[tokio::test]
async fn lakehouse_completion_requires_the_captured_parent_path() {
    for data in [
        json!({"name":"sales_ref"}),
        json!({"name":"sales_ref","path":"Files/other"}),
    ] {
        let outcome = ResourceOutcome::Completed {
            status: 201,
            data: Some(data),
        };
        let mut backend = operations(Ok(outcome.clone()));
        let mut approval = approved();
        let failure = v2_link::execute(
            v2_link::prepare(target(), None, &input(), false).unwrap(),
            &mut backend,
            Some(&mut approval),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.observed, Some(outcome));
        assert_eq!(failure.error.code, "InvalidResponse");
        assert!(
            failure
                .error
                .operation
                .as_ref()
                .unwrap()
                .mutation_may_have_occurred
        );
        assert_eq!(backend.requests.len(), 1);
    }
}

fn datasource() -> v2_link::PreparedLink {
    let target = ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::DataAgent,
        LinkScope::DataAgentStagingDatasources,
    )
    .unwrap();
    v2_link::prepare(
        target,
        None,
        &json!({
            "type":"FabricItem",
            "itemReference":{"referenceType":"ById","workspaceId":WORKSPACE,"itemId":SOURCE}
        }),
        true,
    )
    .unwrap()
}

#[tokio::test]
async fn datasource_completion_requires_a_native_uuid_identity() {
    for id in [" ", "not-a-uuid", ""] {
        let outcome = ResourceOutcome::Completed {
            status: 201,
            data: Some(json!({"id":id})),
        };
        let mut backend = operations(Ok(outcome.clone()));
        let mut approval = approved();
        let failure = v2_link::execute(datasource(), &mut backend, Some(&mut approval))
            .await
            .unwrap_err();
        assert_eq!(failure.observed, Some(outcome));
        assert_eq!(failure.error.code, "InvalidResponse");
        assert!(
            failure
                .error
                .operation
                .as_ref()
                .unwrap()
                .mutation_may_have_occurred
        );
        assert_eq!(backend.requests.len(), 1);
    }
}

#[tokio::test]
async fn completed_datasource_keeps_its_authoritative_resource_identity() {
    let outcome = ResourceOutcome::Completed {
        status: 200,
        data: Some(json!({"id":SOURCE,"type":"FabricItem"})),
    };
    let mut backend = operations(Ok(outcome.clone()));
    let mut approval = approved();
    let receipt = v2_link::execute(datasource(), &mut backend, Some(&mut approval))
        .await
        .unwrap();
    assert_eq!(receipt.outcome, outcome);
    assert_eq!(receipt.metadata()["status"], "succeeded");
    assert_eq!(backend.requests.len(), 1);
}

#[tokio::test]
async fn completed_kql_result_is_not_validated_as_a_files_shortcut() {
    let target = ResolvedLinkTarget::new(
        WORKSPACE,
        ITEM,
        LinkOwner::KqlDatabase,
        LinkScope::KqlTableShortcut {
            name: "sales".into(),
        },
    )
    .unwrap();
    let body = json!({
        "name":"sales",
        "enableQueryAcceleration":false,
        "target":{"oneLake":{"workspaceId":WORKSPACE,"itemId":SOURCE,"path":"Tables/sales"}}
    });
    let outcome = ResourceOutcome::Completed {
        status: 201,
        data: Some(body.clone()),
    };
    let mut backend = operations(Ok(outcome.clone()));
    let mut approval = approved();
    let receipt = v2_link::execute(
        v2_link::prepare(target, None, &body, true).unwrap(),
        &mut backend,
        Some(&mut approval),
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcome, outcome);
    assert_eq!(receipt.metadata()["status"], "succeeded");
}

struct Resolver {
    target: ResolvedLinkTarget,
    requests: usize,
}

impl LinkResolver for Resolver {
    fn resolve<'a>(
        &'a mut self,
        _: &'a str,
    ) -> CommandFuture<'a, Result<ResolvedLinkTarget, MutationError>> {
        self.requests += 1;
        Box::pin(std::future::ready(Ok(self.target.clone())))
    }
}

struct Files {
    bytes: Vec<u8>,
    requests: usize,
}

impl LinkFiles for Files {
    fn read<'a>(
        &'a mut self,
        _: &'a str,
        _: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>> {
        self.requests += 1;
        Box::pin(std::future::ready(Ok(self.bytes.clone())))
    }
}

#[tokio::test]
async fn native_file_is_strict_utf8_json_with_no_duplicate_fields() {
    let command = v2_link::parse_args(["fab-cli", "link", "/demo", "--input", "-"]).unwrap();
    for bytes in [
        vec![0xff],
        br#"{"path":"Files","path":"Files/landing"}"#.to_vec(),
        br#"{"target":{"oneLake":{"itemId":"a","itemId":"b"}}}"#.to_vec(),
        br#"{} {}"#.to_vec(),
        Vec::new(),
    ] {
        let mut resolver = Resolver {
            target: target(),
            requests: 0,
        };
        let mut files = Files { bytes, requests: 0 };
        let error = v2_link::prepare_command(
            &command,
            &mut resolver,
            &mut files,
            NonZeroUsize::new(4096).unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert_eq!(resolver.requests, 1);
        assert_eq!(files.requests, 1);
    }
}

#[tokio::test]
async fn caller_byte_limit_is_checked_even_if_file_adapter_violates_its_contract() {
    let command = v2_link::parse_args(["fab-cli", "link", "/demo", "--input", "-"]).unwrap();
    let mut resolver = Resolver {
        target: target(),
        requests: 0,
    };
    let mut files = Files {
        bytes: vec![b' '; 5],
        requests: 0,
    };
    let error = v2_link::prepare_command(
        &command,
        &mut resolver,
        &mut files,
        NonZeroUsize::new(4).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "BudgetExceeded");
}

#[tokio::test]
async fn wrong_facade_rejects_before_local_input_access() {
    let command = v2_link::parse_args([
        "fab-cli",
        "artifact",
        "data-agent",
        "link",
        "/demo",
        "--input",
        "native.json",
    ])
    .unwrap();
    let mut resolver = Resolver {
        target: target(),
        requests: 0,
    };
    let mut files = Files {
        bytes: serde_json::to_vec(&input()).unwrap(),
        requests: 0,
    };
    let error = v2_link::prepare_command(
        &command,
        &mut resolver,
        &mut files,
        NonZeroUsize::new(4096).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "UnsupportedCapability");
    assert_eq!(files.requests, 0);
}
