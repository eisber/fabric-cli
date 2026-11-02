// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source comparison consumer: real CLI/SDK/shared HTTP storage writes, no live auth.

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_mutations::{
    CommandFuture, MutationUi, NoticeKind, OneLakeResourceKind, OneLakeWriteHost,
    OneLakeWriteTarget,
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor, PublicOrigin,
};
use fabric_sdk_onelake::{OneLakeClient, OneLakeLimits, OneLakePath, OneLakeWriteLimits};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_workspaces::CredentialSelection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
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
        assert_eq!(scopes, ["https://storage.azure.com/.default"]);
        Ok(AccessToken::new(
            "synthetic-offline-one-lake-writes",
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
        let headers: BTreeMap<_, _> = [
            "if-none-match",
            "x-ms-rename-source",
            "content-type",
            "x-ms-content-type",
        ]
        .into_iter()
        .filter_map(|key| {
            request
                .headers()
                .get_str(&key.into())
                .ok()
                .map(|value| (key, value.to_owned()))
        })
        .collect();
        self.requests.lock().unwrap().push(json!({
            "method":match request.method() {
                Method::Get => "get", Method::Put => "put", Method::Patch => "patch", Method::Delete => "delete",
                _ => panic!("unexpected OneLake write method"),
            },
            "url":request.url().as_str(), "headers":headers, "body":Bytes::from(request.body()).as_ref(),
        }));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected OneLake mutation/replay"))
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
    notices: Vec<Value>,
    prompts: Vec<String>,
}
impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, text: &str) {
        self.notices.push(json!({"kind": match kind { NoticeKind::Progress=>"progress", NoticeKind::Warning=>"warning", NoticeKind::Help=>"help" }, "message":text}));
    }
    fn confirm<'a>(&'a mut self, text: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(text.into());
        Box::pin(std::future::ready(self.confirmed))
    }
}

fn target(value: &Value) -> Result<OneLakeWriteTarget, Box<dyn std::error::Error>> {
    let path = value["path"].as_str().ok_or("missing path")?;
    Ok(OneLakeWriteTarget {
        path: OneLakePath::new(
            value["workspace"].as_str().unwrap_or("workspace"),
            value["item"].as_str().unwrap_or("item"),
            path,
        )?,
        item_type: ItemType::parse(value["item_type"].as_str().unwrap_or("Lakehouse"))?,
        kind: match value["kind"].as_str().unwrap_or("file") {
            "file" => OneLakeResourceKind::File,
            "directory" => OneLakeResourceKind::Directory,
            "shortcut" => OneLakeResourceKind::Shortcut,
            "table" => OneLakeResourceKind::Table,
            "undefined" => OneLakeResourceKind::Undefined,
            _ => return Err("invalid resource kind".into()),
        },
        display_path: format!("/Team.Workspace/Lake.Lakehouse/{path}"),
    })
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut replies = VecDeque::new();
    for reply in input["replies"].as_array().ok_or("missing replies")? {
        let mut headers = Headers::new();
        if let Some(values) = reply["headers"].as_object() {
            for (key, value) in values {
                headers.insert(
                    key.clone(),
                    value.as_str().ok_or("invalid header")?.to_owned(),
                );
            }
        }
        let bytes: Vec<u8> = reply["body"]
            .as_array()
            .map(|bytes| {
                bytes
                    .iter()
                    .map(|byte| {
                        byte.as_u64()
                            .and_then(|byte| u8::try_from(byte).ok())
                            .ok_or("invalid byte")
                    })
                    .collect()
            })
            .transpose()?
            .unwrap_or_default();
        replies.push_back(AsyncRawResponse::from_bytes(
            StatusCode::from(
                reply["status"]
                    .as_u64()
                    .and_then(|status| u16::try_from(status).ok())
                    .ok_or("invalid status")?,
            ),
            headers,
            bytes,
        ));
    }
    let network = Arc::new(Network {
        replies: Mutex::new(replies),
        requests: Mutex::new(Vec::new()),
    });
    let adapter = OneLakeHttpExecutor::new(Arc::new(HttpExecutor::new(
        network.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )?));
    let client = OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::OneLake.audience())?,
    );
    let target = target(&input["target"])?;
    let mut ui = Ui {
        confirmed: input["confirm"].as_bool().unwrap_or(true),
        notices: Vec::new(),
        prompts: Vec::new(),
    };
    let mut host = OneLakeWriteHost {
        client: &client,
        executor: &adapter,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
        write_limits: OneLakeWriteLimits::default(),
    };
    let force = input["force"].as_bool().unwrap_or(false);
    let result = match input["command"].as_str().ok_or("missing command")? {
        "mkdir" => {
            host.create_directory(
                &target,
                input["params"].as_bool().unwrap_or(true),
                pending(),
            )
            .await
        }
        "rm" => host.remove(&target, force, pending()).await,
        "mv" => {
            host.rename(&self::target(&input["source"])?, &target, force, pending())
                .await
        }
        "cp" => {
            host.copy_file(&self::target(&input["source"])?, &target, pending())
                .await
        }
        "upload" => {
            let bytes = input["bytes"]
                .as_array()
                .ok_or("missing bytes")?
                .iter()
                .map(|byte| {
                    byte.as_u64()
                        .and_then(|byte| u8::try_from(byte).ok())
                        .ok_or("invalid byte")
                })
                .collect::<Result<Vec<_>, _>>()?;
            host.upload_bytes(
                &target,
                input["source_name"].as_str().unwrap_or("source"),
                &bytes,
                pending(),
            )
            .await
        }
        _ => return Err("unsupported OneLake command".into()),
    };
    Ok(match result {
        Ok(result) => json!({
            "requests":*network.requests.lock().unwrap(), "notices":ui.notices, "prompts":ui.prompts,
            "outputs":result.outputs.iter().map(|output| output.message.clone()).collect::<Vec<_>>(),
            "error":null,
        }),
        Err(error) => {
            let output = error.output();
            json!({
                "requests":*network.requests.lock().unwrap(), "notices":ui.notices, "prompts":ui.prompts,
                "outputs":[], "error":{"code":output.error_code,"message":output.message},
            })
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
