// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual non-force/workspace/tenant removal through CLI, focused SDK and shared HTTP.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CommandFuture, FabricMutationBackend, MutationError, MutationUi, NoticeKind, RemovalCatalog,
    RemovalHost, RemovalSort, RemovalUi,
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor, PublicOrigin,
};
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
            "offline-removal-consumer",
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
        assert_eq!(request.method(), Method::Delete);
        self.requests
            .lock()
            .unwrap()
            .push(json!({"method":"delete","url":request.url().as_str()}));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected removal request/replay"))
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
    confirm: bool,
    selected: Vec<String>,
    choices: Vec<Value>,
    prompts: Vec<String>,
    notices: Vec<Value>,
}
impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, text: &str) {
        self.notices.push(json!({"kind":match kind {NoticeKind::Progress=>"progress",NoticeKind::Warning=>"warning",NoticeKind::Help=>"help"},"message":text}));
    }
    fn confirm<'a>(&'a mut self, text: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(text.into());
        Box::pin(std::future::ready(self.confirm))
    }
}
impl RemovalUi for Ui {
    fn select<'a>(
        &'a mut self,
        prompt: &'a str,
        names: &'a [String],
    ) -> CommandFuture<'a, Vec<String>> {
        self.choices.push(json!({"prompt":prompt,"names":names}));
        Box::pin(std::future::ready(self.selected.clone()))
    }
}
struct Catalog {
    workspaces: Vec<Arc<Element>>,
    items: Vec<Arc<Element>>,
    reads: Vec<String>,
}
impl RemovalCatalog for Catalog {
    fn workspaces<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        self.reads.push("workspaces".into());
        Box::pin(std::future::ready(Ok(self.workspaces.clone())))
    }
    fn items<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        self.reads.push("items".into());
        Box::pin(std::future::ready(Ok(self.items.clone())))
    }
}
fn selected(input: &Value) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(input
        .as_array()
        .ok_or("selected must be names")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or("invalid selected name")
        })
        .collect::<Result<_, _>>()?)
}
async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let tenant = Element::tenant("Tenant", "tenant");
    let workspace = Element::child(
        "Team",
        Some("workspace".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Arc::clone(&tenant),
    )?;
    let mut items = Vec::new();
    for item in input["items"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let kind = if item["type"] == "Folder" {
            NodeKind::Folder
        } else {
            NodeKind::Item(ItemType::parse(
                item["type"].as_str().ok_or("missing item type")?,
            )?)
        };
        items.push(Element::child(
            item["name"].as_str().ok_or("missing name")?,
            Some(item["id"].as_str().ok_or("missing id")?.into()),
            kind,
            Arc::clone(&workspace),
        )?);
    }
    let mut workspaces = Vec::new();
    for entry in input["workspaces"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        workspaces.push(Element::child(
            entry["name"].as_str().ok_or("missing workspace name")?,
            Some(entry["id"].as_str().ok_or("missing workspace id")?.into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            Arc::clone(&tenant),
        )?);
    }
    let mut replies = VecDeque::new();
    for reply in input["responses"]
        .as_array()
        .ok_or("missing response script")?
    {
        let status = reply["status"]
            .as_u64()
            .and_then(|status| u16::try_from(status).ok())
            .ok_or("invalid status")?;
        replies.push_back(AsyncRawResponse::from_bytes(
            StatusCode::from(status),
            Headers::new(),
            if status == 403 {
                br#"{"errorCode":"Forbidden","message":"denied"}"#.to_vec()
            } else {
                Vec::new()
            },
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
        confirm: input["confirm"].as_bool().unwrap_or(true),
        selected: selected(&input["selected"])?,
        choices: Vec::new(),
        prompts: Vec::new(),
        notices: Vec::new(),
    };
    let mut catalog = Catalog {
        workspaces,
        items,
        reads: Vec::new(),
    };
    let mut host = RemovalHost {
        operations: &mut backend,
        ui: &mut ui,
        catalog: &mut catalog,
        sort: if input["sort"] == "bytype" {
            RemovalSort::ByType
        } else {
            RemovalSort::ByName
        },
    };
    let result = if input["command"] == "tenant" {
        host.tenant(&tenant).await
    } else {
        host.workspace(
            &workspace,
            input["force"].as_bool().unwrap_or(false),
            input["hard"].as_bool().unwrap_or(false),
        )
        .await
    };
    let (result, error) = match result {
        Ok(result) => (result, None),
        Err(error) => (*error.partial, Some(error.error.code.clone())),
    };
    Ok(json!({
        "requests":*network.requests.lock().unwrap(),"choices":ui.choices,"prompts":ui.prompts,"notices":ui.notices,"catalog_reads":catalog.reads,
        "outputs":result.outputs.iter().map(|output|output.message.clone()).collect::<Vec<_>>(),
        "cache":result.effects.iter().map(|effect|match effect {fabric_cli_mutations::CacheEffect::Deleted {target}=>target.name(),_=>panic!("unexpected removal cache effect")}).collect::<Vec<_>>(),
        "error":error,
    }))
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?).await?);
    }
    Ok(())
}
