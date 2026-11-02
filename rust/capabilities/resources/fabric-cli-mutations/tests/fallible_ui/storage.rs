// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Ui, input_error};
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_mutations::{
    OneLakeResourceKind, OneLakeWriteError, OneLakeWriteHost, OneLakeWriteTarget,
};
use fabric_sdk_http::PublicOrigin;
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeExecutionError, OneLakeExecutor, OneLakePath, OneLakeRequest,
    OneLakeResponse,
};
use fabric_sdk_workspaces::CredentialSelection;
use std::{
    collections::BTreeMap,
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
            "offline-prompt-test",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Executor(Mutex<Vec<String>>);
impl OneLakeExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        self.0.lock().unwrap().push(request.url().to_string());
        Box::pin(std::future::ready(Ok(OneLakeResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Vec::new(),
        })))
    }
}

fn target(path: &str) -> OneLakeWriteTarget {
    OneLakeWriteTarget {
        path: OneLakePath::new("ws", "item", path).unwrap(),
        item_type: ItemType::parse("Lakehouse").unwrap(),
        kind: OneLakeResourceKind::File,
        display_path: path.into(),
    }
}

#[tokio::test]
async fn storage_delete_and_move_keep_input_failures_typed_and_force_explicit() {
    let client = OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::OneLake.audience()).unwrap(),
    );
    for moving in [false, true] {
        for force in [false, true] {
            for failed in [false, true] {
                let executor = Executor::default();
                let mut ui = Ui::new(if failed {
                    Err(input_error())
                } else {
                    Ok(false)
                });
                let mut host = OneLakeWriteHost {
                    client: &client,
                    executor: &executor,
                    ui: &mut ui,
                    read_limits: Default::default(),
                    write_limits: Default::default(),
                };
                let result = if moving {
                    host.rename(&target("Files/from"), &target("Files/to"), force, pending())
                        .await
                } else {
                    host.remove(&target("Files/from"), force, pending()).await
                };
                if force {
                    assert!(!result.unwrap().declined);
                } else if failed {
                    assert_eq!(
                        result.unwrap_err(),
                        OneLakeWriteError::Command(input_error())
                    );
                } else {
                    let result = result.unwrap();
                    assert!(result.declined);
                    assert!(result.outcome.is_none());
                    assert!(result.outputs.is_empty());
                }
                assert_eq!(executor.0.lock().unwrap().len(), usize::from(force));
                assert_eq!(ui.questions.len(), usize::from(!force));
            }
        }
    }
}
