// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual Report parser -> shared host -> SDK -> scripted HTTP contracts.

use super::*;
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding,
        CreateCommandError, MAX_BYTES, PreparedCreateCommand,
    },
    v2_create_report::ReportCreateBinding,
};
use fabric_sdk::{
    Client,
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
};
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::Arc,
};

struct Listing {
    body: Value,
    calls: AtomicUsize,
}

impl Listing {
    fn new(rows: Value) -> Self {
        Self {
            body: json!({"value":rows}),
            calls: AtomicUsize::new(0),
        }
    }
}

impl ItemExecutor for Listing {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        assert_eq!(request.method(), "GET");
        assert_eq!(request.url().path(), format!("/v1/workspaces/{WS}/items"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(ItemResponse {
            status: 200,
            body: serde_json::to_vec(&self.body).unwrap(),
            headers: BTreeMap::new(),
        })))
    }
}

struct Approval {
    raw: Vec<u8>,
    result: Result<(), CreateApprovalError>,
    calls: usize,
    pending: bool,
}

impl Approval {
    fn allow(raw: Vec<u8>) -> Self {
        Self {
            raw,
            result: Ok(()),
            calls: 0,
            pending: false,
        }
    }
}

impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls += 1;
        assert_eq!(request.plan().target().parent_id().as_str(), WS);
        assert_eq!(request.plan().target().name(), "Report");
        assert_eq!(request.plan().target().item_type(), "Report");
        assert_eq!(request.context().selection(), &selection(Audience::Fabric));
        assert_eq!(request.raw_input(), Some(self.raw.as_slice()));
        assert_eq!(request.command().destination, PATH);
        if self.pending {
            Box::pin(pending())
        } else {
            Box::pin(std::future::ready(self.result))
        }
    }
}

fn client(audience: Audience) -> (Client, Arc<Credential>) {
    let credential = Arc::new(Credential::default());
    (
        Client::new(credential.clone(), selection(audience)),
        credential,
    )
}

fn prepare(cwd: &Path, args: &[&str], raw: &[u8]) -> PreparedCreateCommand {
    let command = v2_create::parse_args(
        ["create", PATH, "--input", "-"]
            .into_iter()
            .chain(args.iter().copied()),
    )
    .unwrap();
    PreparedCreateCommand::preflight(command, &ReportCreateBinding, cwd, &mut &raw[..], false)
        .unwrap()
}

fn metadata() -> Value {
    json!({"id":REPORT,"workspaceId":WS,"displayName":"Report","type":"Report","future":null})
}

#[tokio::test]
async fn full_host_preserves_raw_body_binary_extensions_and_projected_receipt_identity() {
    let temp = tempfile::tempdir().unwrap();
    let raw = format!(
        " \r\n{}\r\n",
        serde_json::to_string_pretty(&body()).unwrap()
    )
    .into_bytes();
    let prepared = prepare(temp.path(), &["--project", "future"], &raw);
    let (client, _) = client(Audience::Fabric);
    let listing = Listing::new(json!([]));
    let wire = Wire::new(vec![response(201, metadata())]);
    let mut approval = Approval::allow(raw.clone());
    let delivery = prepared
        .execute(
            &ReportCreateBinding,
            &client,
            &listing,
            &wire,
            &mut approval,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(approval.calls, 1);
    assert_eq!(listing.calls.load(Ordering::SeqCst), 1);
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(delivery.report().value()["meta"]["status"], "completed");
    let mut output = Vec::new();
    delivery.deliver(&mut output).unwrap();
    let receipt: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(receipt["data"], Value::Null);
    assert_eq!(receipt["meta"]["resourceId"], REPORT);
    assert_eq!(receipt["meta"]["workspaceId"], WS);
    assert_eq!(receipt["meta"]["operationHandle"], Value::Null);
    assert_eq!(receipt["meta"]["remoteReplayed"], false);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, ResourceMethod::Post);
    assert_eq!(
        requests[0].1,
        format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/reports")
    );
    assert_eq!(requests[0].2, raw);
}

#[tokio::test]
async fn denial_or_unavailable_approval_and_wrong_context_send_no_post() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for refusal in [
        CreateApprovalError::Denied,
        CreateApprovalError::Unavailable,
    ] {
        let prepared = prepare(temp.path(), &[], &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(json!([]));
        let wire = Wire::new(vec![]);
        let mut approval = Approval::allow(raw.clone());
        approval.result = Err(refusal);
        assert!(matches!(prepared.execute(
            &ReportCreateBinding, &client, &listing, &wire, &mut approval, pending(),
        ).await, Err(CreateCommandError::Approval(error)) if error == refusal));
        assert_eq!(approval.calls, 1);
        assert!(wire.requests.lock().unwrap().is_empty());
    }
    let prepared = prepare(temp.path(), &[], &raw);
    let (client, credential) = client(Audience::OneLake);
    let listing = Listing::new(json!([]));
    let wire = Wire::new(vec![]);
    let mut approval = Approval::allow(raw);
    assert!(matches!(
        prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                &wire,
                &mut approval,
                pending(),
            )
            .await,
        Err(CreateCommandError::Invalid)
    ));
    assert_eq!(listing.calls.load(Ordering::SeqCst), 0);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert_eq!(approval.calls, 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn original_interruption_cancels_pending_approval_before_any_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    let prepared = prepare(temp.path(), &["--timeout", "1ms"], &raw);
    let (client, _) = client(Audience::Fabric);
    let listing = Listing::new(json!([]));
    let wire = Wire::new(vec![]);
    let mut approval = Approval::allow(raw);
    approval.pending = true;
    let result = prepared
        .execute(
            &ReportCreateBinding,
            &client,
            &listing,
            &wire,
            &mut approval,
            tokio::time::sleep(Duration::from_millis(5)),
        )
        .await;
    assert!(matches!(result, Err(CreateCommandError::Interrupted)));
    assert_eq!(approval.calls, 1);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn exact_existing_skip_is_unchanged_but_wrong_kind_and_ambiguity_never_write() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for rows in [
        json!([metadata()]),
        json!([{"id":REPORT,"displayName":"Report","type":"Notebook"}]),
        json!([metadata(), metadata()]),
        json!([{"id":REPORT,"displayName":"Report","type":"Report","workspaceId":OP}]),
    ] {
        let prepared = prepare(temp.path(), &["--if-exists", "skip"], &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(rows.clone());
        let wire = Wire::new(vec![]);
        let mut approval = Approval::allow(raw.clone());
        let result = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                &wire,
                &mut approval,
                pending(),
            )
            .await;
        if rows == json!([metadata()]) {
            let delivery = result.unwrap();
            assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
            assert_eq!(
                delivery.report().value()["meta"]["contentEqualityClaimed"],
                false
            );
            assert_eq!(delivery.report().value()["meta"]["resourceId"], REPORT);
        } else {
            assert!(result.is_err());
            assert_eq!(approval.calls, 0);
        }
        assert!(wire.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn malformed_created_and_accepted_responses_never_claim_complete_or_replay() {
    let mut foreign_acceptance = accepted();
    foreign_acceptance.headers.insert(
        "Location".into(),
        format!("https://example.invalid/v1/operations/{OP}"),
    );
    let mut bad_id = accepted();
    bad_id
        .headers
        .insert("x-ms-operation-id".into(), "not-an-operation-id".into());
    let replies = vec![
        response(201, Value::Null),
        response(201, json!({})),
        response(
            201,
            json!({"id":REPORT,"workspaceId":OP,"displayName":"Report","type":"Report"}),
        ),
        response(
            201,
            json!({"id":REPORT,"workspaceId":WS,"displayName":"Wrong","type":"Report"}),
        ),
        response(
            201,
            json!({"id":REPORT,"workspaceId":WS,"displayName":"Report","type":"Notebook"}),
        ),
        OperationResponse {
            status: 201,
            body: b"{".to_vec(),
            headers: BTreeMap::new(),
        },
        OperationResponse {
            status: 202,
            body: Vec::new(),
            headers: BTreeMap::new(),
        },
        foreign_acceptance,
        bad_id,
    ];
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for reply in replies {
        let prepared = prepare(temp.path(), &[], &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(json!([]));
        let wire = Wire::new(vec![reply]);
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                &wire,
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_ne!(delivery.report().exit_code(), 0);
        assert_ne!(delivery.report().value()["meta"]["status"], "completed");
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        assert_eq!(
            delivery.report().value()["meta"]["mutationMayHaveOccurred"],
            true
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn genuine_acceptance_and_lro_result_keep_operation_and_resource_ids_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for wait in [false, true] {
        let prepared = prepare(temp.path(), if wait { &["--wait"] } else { &[] }, &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(json!([]));
        let wire = Wire::new(vec![
            accepted(),
            response(200, json!({"status":"Succeeded"})),
            response(200, metadata()),
        ]);
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                &wire,
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        let receipt = delivery.report().value();
        assert_eq!(delivery.report().exit_code(), 0);
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["completed"], wait);
        assert_eq!(
            receipt["meta"]["resourceId"],
            if wait { json!(REPORT) } else { Value::Null }
        );
        assert_eq!(
            wire.requests.lock().unwrap().len(),
            if wait { 3 } else { 1 }
        );
        if wait {
            assert_eq!(
                wire.waits.lock().unwrap().as_slice(),
                [Duration::from_secs(2)]
            );
        }
    }
}

#[tokio::test]
async fn poll_failure_and_response_budget_retains_known_handle_or_resource_uncertainty() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for replies in [
        vec![
            accepted(),
            response(500, json!({"errorCode":"OfflineFailure"})),
        ],
        vec![OperationResponse {
            status: 201,
            body: vec![b' '; MAX_BYTES + 1],
            headers: BTreeMap::new(),
        }],
    ] {
        let accepted_first = replies[0].status == 202;
        let prepared = prepare(temp.path(), &["--wait"], &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(json!([]));
        let wire = Wire::new(replies);
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                &wire,
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_ne!(delivery.report().exit_code(), 0);
        assert!(delivery.report().error().is_some());
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        assert_eq!(
            delivery.report().value()["meta"]["mutationMayHaveOccurred"],
            true
        );
        assert_eq!(delivery.report().value()["meta"]["resourceId"], Value::Null);
        assert_eq!(
            delivery.report().value()["meta"]["operationHandle"],
            if accepted_first {
                json!(format!("/operations/{OP}"))
            } else {
                Value::Null
            },
        );
        assert_eq!(
            wire.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.0 == ResourceMethod::Post)
                .count(),
            1
        );
    }
}

struct CancelAtPoll {
    wire: Wire,
    cancel: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl OperationExecutor for &CancelAtPoll {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move { (&self.wire).execute(request).await })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move {
            self.cancel
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            pending().await
        })
    }
}

#[tokio::test]
async fn cancellation_after_acceptance_keeps_handle_without_remote_cancel_or_replay() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    let prepared = prepare(temp.path(), &["--wait"], &raw);
    let (client, _) = client(Audience::Fabric);
    let listing = Listing::new(json!([]));
    let (cancel, interrupted) = tokio::sync::oneshot::channel();
    let wire = CancelAtPoll {
        wire: Wire::new(vec![accepted()]),
        cancel: Mutex::new(Some(cancel)),
    };
    let mut approval = Approval::allow(raw);
    let delivery = prepared
        .execute(
            &ReportCreateBinding,
            &client,
            &listing,
            &wire,
            &mut approval,
            async {
                interrupted.await.unwrap();
            },
        )
        .await
        .unwrap();
    assert_ne!(delivery.report().exit_code(), 0);
    assert_eq!(delivery.report().value()["error"]["code"], "Cancelled");
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(delivery.report().value()["meta"]["resourceId"], Value::Null);
    assert_eq!(
        delivery.report().value()["meta"]["mutationMayHaveOccurred"],
        true
    );
    assert_eq!(delivery.report().value()["meta"]["remoteCancelled"], false);
    assert_eq!(delivery.report().value()["meta"]["remoteReplayed"], false);
    assert_eq!(wire.wire.requests.lock().unwrap().len(), 1);
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("offline output failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("offline flush failure"))
    }
}

#[tokio::test]
async fn delivery_failure_after_acceptance_retains_handle_effect_and_no_clobber() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for output_file in [false, true] {
        let output = temp.path().join("receipt.json");
        let args = if output_file {
            vec!["-o", output.to_str().unwrap()]
        } else {
            vec![]
        };
        let prepared = prepare(temp.path(), &args, &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(json!([]));
        let wire = Wire::new(vec![accepted()]);
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                &wire,
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        if output_file {
            std::fs::write(&output, b"do not replace").unwrap();
        }
        let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut BrokenWriter)
        else {
            panic!("local failure must retain original report");
        };
        assert_eq!(report.value()["meta"]["status"], "accepted");
        assert_eq!(
            report.value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(report.value()["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(report.value()["meta"]["remoteReplayed"], false);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        if output_file {
            assert_eq!(std::fs::read(output).unwrap(), b"do not replace");
        }
    }
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("no input read permitted")
    }
}

#[test]
fn host_rejects_wrong_owner_profile_stdin_conflicts_invalid_input_and_policy_locally() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "create",
            "/11111111-1111-1111-1111-111111111111.Workspace/Report.Notebook",
            "--input",
            "-",
        ],
        vec!["create", PATH, "--profile", "named", "--input", "-"],
    ] {
        let command = v2_create::parse_args(args).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &ReportCreateBinding,
                temp.path(),
                &mut ForbiddenRead,
                false,
            )
            .is_err()
        );
    }
    let parsed = command(false);
    assert!(
        PreparedCreateCommand::preflight(
            parsed,
            &ReportCreateBinding,
            temp.path(),
            &mut ForbiddenRead,
            true,
        )
        .is_err()
    );
    for bytes in [
        b"".as_slice(),
        b"null",
        b"{}",
        b"{",
        br#"{"displayName":"Report","displayName":"Report"}"#,
    ] {
        let command = command(false);
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &ReportCreateBinding,
                temp.path(),
                &mut &bytes[..],
                false,
            )
            .is_err()
        );
    }
    let raw = serde_json::to_vec(&body()).unwrap();
    for policy in ["overwrite", "duplicate"] {
        let command =
            v2_create::parse_args(["create", PATH, "--input", "-", "--if-exists", policy]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &ReportCreateBinding,
                temp.path(),
                &mut &raw[..],
                false,
            )
            .is_err()
        );
    }
    let mut oversized = raw.clone();
    oversized.resize(MAX_BYTES + 1, b' ');
    assert!(
        PreparedCreateCommand::preflight(
            command(false),
            &ReportCreateBinding,
            temp.path(),
            &mut &oversized[..],
            false,
        )
        .is_err()
    );
    let prepared_plan = command(false)
        .plan(
            target(),
            Preflight::Leaf(LeafState::Missing),
            &capabilities(),
            Some(body()),
            &validator(),
        )
        .unwrap();
    let mut changed = body();
    changed["description"] = json!("substitution");
    assert!(
        ReportCreateBinding
            .operation(
                &prepared_plan,
                Some(&serde_json::to_vec(&changed).unwrap()),
                false,
            )
            .is_err()
    );
}
