// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Write-command outputs and explicit partial-storage evidence, separate from SDK tests.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_mutations::{
    CommandFuture, MutationUi, NoticeKind, OneLakeResourceKind, OneLakeWriteError,
    OneLakeWriteHost, OneLakeWriteTarget,
};
use fabric_sdk_http::PublicOrigin;
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutionError, OneLakeExecutor, OneLakeLimits,
    OneLakePath, OneLakeRequest, OneLakeResponse, OneLakeWriteLimits, OneLakeWriteOutcome,
    OneLakeWriteStep,
};
use fabric_sdk_workspaces::CredentialSelection;
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new(
            "offline-write-command",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<OneLakeResponse>>,
    requests: Mutex<Vec<String>>,
}
impl OneLakeExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        self.requests
            .lock()
            .unwrap()
            .push(request.url().to_string());
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected write");
        Box::pin(std::future::ready(Ok(response)))
    }
}
#[derive(Default)]
struct Ui {
    confirmed: bool,
    prompts: Vec<String>,
    notices: Vec<(NoticeKind, String)>,
}
impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, text: &str) {
        self.notices.push((kind, text.into()));
    }
    fn confirm<'a>(&'a mut self, text: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(text.into());
        Box::pin(std::future::ready(self.confirmed))
    }
}
fn client() -> OneLakeClient {
    OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::OneLake.audience()).unwrap(),
    )
}
fn executor(statuses: &[u16]) -> Executor {
    Executor {
        responses: Mutex::new(
            statuses
                .iter()
                .map(|&status| OneLakeResponse {
                    status,
                    body: Vec::new(),
                    headers: BTreeMap::new(),
                })
                .collect(),
        ),
        ..Executor::default()
    }
}
fn target(kind: OneLakeResourceKind) -> OneLakeWriteTarget {
    OneLakeWriteTarget {
        path: OneLakePath::new("ws", "item", "Files/target").unwrap(),
        item_type: ItemType::parse("Lakehouse").unwrap(),
        kind,
        display_path: "/Team.Workspace/Lake.Lakehouse/Files/target".into(),
    }
}
fn host<'a>(
    client: &'a OneLakeClient,
    executor: &'a Executor,
    ui: &'a mut Ui,
) -> OneLakeWriteHost<'a> {
    OneLakeWriteHost {
        client,
        executor,
        ui,
        read_limits: OneLakeLimits::default(),
        write_limits: OneLakeWriteLimits::default(),
    }
}

#[tokio::test]
async fn accepted_delete_source_message_is_separate_from_actual_acceptance_evidence() {
    let client = client();
    let executor = executor(&[202]);
    let mut ui = Ui::default();
    let result = host(&client, &executor, &mut ui)
        .remove(&target(OneLakeResourceKind::File), true, pending())
        .await
        .unwrap();
    assert_eq!(result.outcome, Some(OneLakeWriteOutcome::Accepted));
    assert_eq!(
        result.outputs[0].message.as_deref(),
        Some("'/Team.Workspace/Lake.Lakehouse/Files/target' is being deleted")
    );
}

#[tokio::test]
async fn source_done_output_does_not_erase_accepted_upload_stages() {
    let client = client();
    let executor = executor(&[201, 202, 202]);
    let mut ui = Ui::default();
    let result = host(&client, &executor, &mut ui)
        .upload_bytes(
            &target(OneLakeResourceKind::File),
            "source",
            b"data",
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.outputs[0].message.as_deref(), Some("Done"));
    let upload = result.upload.unwrap();
    assert_eq!(upload.append, Some(OneLakeWriteOutcome::Accepted));
    assert_eq!(upload.flush, Some(OneLakeWriteOutcome::Accepted));
}

#[tokio::test]
async fn failed_flush_preserves_partial_effects_and_never_prints_done() {
    let client = client();
    let executor = executor(&[201, 202, 403]);
    let mut ui = Ui::default();
    let error = host(&client, &executor, &mut ui)
        .upload_bytes(
            &target(OneLakeResourceKind::File),
            "source",
            b"data",
            pending(),
        )
        .await
        .unwrap_err();
    let OneLakeWriteError::Storage(error) = error else {
        panic!("lost storage failure");
    };
    assert_eq!(
        error.kind,
        OneLakeError::Http {
            status: 403,
            code: None,
            raw_service_error: None,
        }
    );
    assert_eq!(
        error.completed_steps,
        [OneLakeWriteStep::CreateFile, OneLakeWriteStep::Append]
    );
    assert!(error.mutation_may_have_occurred);
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn declining_delete_or_move_never_dispatches_or_mutates() {
    let client = client();
    let executor = Executor::default();
    let mut ui = Ui::default();
    let target = target(OneLakeResourceKind::File);
    let mut host = host(&client, &executor, &mut ui);
    assert!(
        host.remove(&target, false, pending())
            .await
            .unwrap()
            .declined
    );
    assert!(
        host.rename(&target, &target, false, pending())
            .await
            .unwrap()
            .declined
    );
    assert!(executor.requests.lock().unwrap().is_empty());
    assert_eq!(ui.prompts, ["Are you sure?", "Are you sure?"]);
}

#[tokio::test]
async fn shortcut_remove_does_not_fall_back_to_recursive_dfs_delete() {
    let client = client();
    let executor = Executor::default();
    let mut ui = Ui::default();
    let error = host(&client, &executor, &mut ui)
        .remove(&target(OneLakeResourceKind::Shortcut), true, pending())
        .await
        .unwrap_err();
    assert!(matches!(error,OneLakeWriteError::Command(error) if error.code=="NotSupported"));
    assert!(executor.requests.lock().unwrap().is_empty());
    assert!(ui.prompts.is_empty());
}

#[tokio::test]
async fn cancelled_write_before_dispatch_preserves_no_side_effects() {
    let client = client();
    let executor = Executor::default();
    let mut ui = Ui::default();
    let error = host(&client, &executor, &mut ui)
        .create_directory(
            &target(OneLakeResourceKind::Directory),
            true,
            std::future::ready(()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error,OneLakeWriteError::Storage(error) if error.kind==OneLakeError::Cancelled && !error.mutation_may_have_occurred)
    );
    assert!(executor.requests.lock().unwrap().is_empty());
}
