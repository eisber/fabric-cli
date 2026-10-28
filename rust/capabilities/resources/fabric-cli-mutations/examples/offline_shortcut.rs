// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual Fabric-audience shortcut CLI/SDK/operation/shared-HTTP offline consumer.

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_mutations::{
    CommandFuture, FabricMutationBackend, MutationResult, MutationUi, NoticeKind,
    OneLakeShortcutHost, ShortcutCreateDestination, ShortcutCreateOptions,
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor, PublicOrigin,
};
use fabric_sdk_onelake::{OneLakePath, shortcuts::ShortcutPath};
use fabric_sdk_operations::OperationLimits;
use fabric_sdk_workspaces::CredentialSelection;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    io::{self, BufRead},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new(
            "synthetic-fabric-shortcut",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Value>>,
}
#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let bytes = Bytes::from(request.body());
        let payload = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("non-JSON shortcut payload")
        };
        self.requests.lock().unwrap().push(json!({
            "method":match request.method() { Method::Get=>"get",Method::Post=>"post",Method::Delete=>"delete",_=>panic!("unexpected shortcut method") },
            "url":request.url().as_str(),"payload":payload,
        }));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected shortcut request/replay"))
    }
}
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        assert_eq!(delay, Duration::from_secs(240));
        Box::pin(pending())
    }
}
struct Ui {
    confirmed: bool,
    prompts: Vec<String>,
    notices: Vec<Value>,
}
impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        self.notices.push(json!({"kind":match kind { NoticeKind::Progress=>"progress",NoticeKind::Warning=>"warning",NoticeKind::Help=>"help" },"message":message}));
    }
    fn confirm<'a>(&'a mut self, message: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(message.into());
        Box::pin(std::future::ready(self.confirmed))
    }
}
async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut replies = VecDeque::new();
    for reply in input["replies"].as_array().ok_or("missing replies")? {
        replies.push_back(AsyncRawResponse::from_bytes(
            StatusCode::from(
                reply["status"]
                    .as_u64()
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or("invalid status")?,
            ),
            Headers::new(),
            serde_json::to_vec(&reply["data"])?,
        ));
    }
    let network = Arc::new(Network {
        replies: Mutex::new(replies),
        requests: Mutex::new(Vec::new()),
    });
    let http = HttpExecutor::new(
        network.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )?;
    let mut backend = FabricMutationBackend::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::Fabric.audience())?,
        &http,
        OperationLimits::default(),
        pending(),
    );
    let mut ui = Ui {
        confirmed: input["confirm"].as_bool().unwrap_or(true),
        prompts: Vec::new(),
        notices: Vec::new(),
    };
    let mut host = OneLakeShortcutHost {
        operations: &mut backend,
        ui: &mut ui,
    };
    let destination = ShortcutCreateDestination {
        workspace: "workspace".into(),
        item: "item".into(),
        local_path: input["path"].as_str().ok_or("missing path")?.into(),
        display_path: input["display_path"]
            .as_str()
            .unwrap_or(input["path"].as_str().unwrap())
            .into(),
        item_type: ItemType::parse(input["item_type"].as_str().unwrap_or("Lakehouse"))?,
    };
    let result = match input["command"].as_str().unwrap_or("ln") {
        "ln" => {
            let target = input
                .get("target")
                .filter(|value| !value.is_null())
                .map(|value| {
                    OneLakePath::new(
                        "target-workspace",
                        "target-item",
                        value.as_str().unwrap_or(""),
                    )
                })
                .transpose()?;
            let tokens = input
                .get("input")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_array()
                        .ok_or("input is not tokens")?
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .map(str::to_owned)
                                .ok_or("invalid input token")
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?;
            host.create(
                &destination,
                &ShortcutCreateOptions {
                    target,
                    input: tokens,
                    shortcut_type: input["type"].as_str().unwrap_or("oneLake").into(),
                    force: input["force"].as_bool().unwrap_or(false),
                },
            )
            .await
        }
        "rm" => {
            host.remove(
                &ShortcutPath::new(
                    "workspace",
                    "item",
                    input["folder"].as_str().ok_or("missing folder")?,
                    input["name"].as_str().ok_or("missing name")?,
                )?,
                input["display_name"].as_str().unwrap_or("Chosen.Shortcut"),
                input["force"].as_bool().unwrap_or(false),
                input["verbose"].as_bool().unwrap_or(true),
            )
            .await
        }
        "set" => host
            .update(
                &ShortcutPath::new(
                    "workspace",
                    "item",
                    input["folder"].as_str().unwrap_or("Files"),
                    input["name"].as_str().unwrap_or("Chosen"),
                )?,
                input["query"].as_str().ok_or("missing query")?,
                input["value"].as_str().ok_or("missing value")?,
                input["force"].as_bool().unwrap_or(false),
            )
            .await
            .map(|result| MutationResult {
                outputs: result.outputs,
                declined: result.declined,
                ..MutationResult::default()
            })
            .map_err(|error| *error.error),
        _ => return Err("unsupported shortcut command".into()),
    };
    Ok(match result {
        Ok(result) => {
            json!({"requests":*network.requests.lock().unwrap(),"notices":ui.notices,"prompts":ui.prompts,"outputs":result.outputs.iter().map(|output|output.message.clone()).collect::<Vec<_>>(),"error":null})
        }
        Err(error) => {
            json!({"requests":*network.requests.lock().unwrap(),"notices":ui.notices,"prompts":ui.prompts,"outputs":[],"error":{"code":error.code,"message":error.message}})
        }
    })
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?).await?);
    }
    Ok(())
}
