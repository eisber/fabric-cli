// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real argv -> transfer adapter -> existing OneLake SDK/shared HTTP consumer.

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use clap::Command;
use fabric_cli_context::{
    Catalog, Element, HierarchyCatalog, HomeContext, ItemType, LocalKind, LocalMetadata, NodeKind,
    OneLakeProperties, PathElement, PathInput, PathResolver, ResolveOptions, WorkspaceType,
};
use fabric_cli_mutations::{
    CommandFuture, MutationError, MutationUi, NoticeKind, OneLakeWriteError, OneLakeWriteHost,
    frontdoor::{
        ResourceContext, ResourceFiles, ResourceTransferHost, command_from_matches,
        resource_subcommands,
    },
};
use fabric_cli_output::{Indent, OutputEnvelope, OutputStream, Timestamp};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor, PublicOrigin,
};
use fabric_sdk_onelake::{OneLakeClient, OneLakeLimits, OneLakeWriteLimits};
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
            "offline-resource-transfer",
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
            "method": match request.method() {
                Method::Get => "get", Method::Put => "put", Method::Patch => "patch",
                _ => panic!("unexpected transfer method"),
            },
            "url":request.url().as_str(), "headers":headers,
            "body":Bytes::from(request.body()).as_ref(),
        }));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected transfer/replay"))
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

struct Ui {
    confirmed: bool,
    notices: Vec<Value>,
    prompts: Vec<String>,
}
impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        self.notices.push(json!({"kind":match kind {
            NoticeKind::Progress => "progress", NoticeKind::Warning => "warning",
            NoticeKind::Help => "help",
        },"message":message}));
    }
    fn confirm<'a>(&'a mut self, message: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(message.into());
        Box::pin(std::future::ready(self.confirmed))
    }
}

struct Context<'a> {
    resolver: PathResolver<'a, HierarchyCatalog>,
    local_kind: LocalKind,
}
impl ResourceContext for Context<'_> {
    fn resolve(
        &mut self,
        tokens: &[String],
        options: ResolveOptions,
    ) -> Result<Arc<PathElement>, MutationError> {
        if tokens == ["local.csv"] && options.supports_local_path {
            return Ok(PathElement::local(LocalMetadata {
                path: "local.csv".into(),
                name: "local.csv".into(),
                id: "local-fixture".into(),
                kind: self.local_kind,
            }));
        }
        let tokens: Vec<_> = tokens.iter().map(String::as_str).collect();
        self.resolver
            .resolve(
                PathInput::Tokens(&tokens),
                ResolveOptions {
                    supports_local_path: false,
                    ..options
                },
            )
            .map(|result| result.element)
            .map_err(|failure| MutationError::new(&failure.error.code, failure.error.message))
    }
}

struct Files {
    bytes: Vec<u8>,
    reads: usize,
}
impl ResourceFiles for Files {
    fn read<'a>(
        &'a mut self,
        file: &'a LocalMetadata,
        max_bytes: usize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>> {
        assert_eq!(file.path, "local.csv");
        self.reads += 1;
        let result = if self.bytes.len() > max_bytes {
            Err(MutationError::new(
                "InvalidInput",
                "Offline file exceeded its read bound",
            ))
        } else {
            Ok(self.bytes.clone())
        };
        Box::pin(std::future::ready(result))
    }
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let tokens: Vec<_> = std::iter::once("fab-rs")
        .chain(
            input["argv"]
                .as_array()
                .ok_or("missing argv")?
                .iter()
                .map(|value| value.as_str().expect("non-string argv")),
        )
        .collect();
    let matches = Command::new("fab-rs")
        .subcommands(resource_subcommands())
        .try_get_matches_from(tokens)?;
    let command = command_from_matches(&matches)?;
    let tenant = Element::tenant("Tenant", "tenant");
    let workspace = Element::child(
        "Team",
        Some("workspace".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant.clone(),
    )?;
    let item = Element::child(
        "Lake",
        Some("item".into()),
        NodeKind::Item(ItemType::parse("Lakehouse")?),
        workspace.clone(),
    )?;
    let catalog = HierarchyCatalog::new(
        Catalog::new(vec![workspace, item]),
        Vec::new(),
        vec![
            (
                "workspace/item/Files".into(),
                OneLakeProperties {
                    status_code: 200,
                    resource_type: Some("directory".into()),
                    is_shortcut: false,
                },
            ),
            (
                "workspace/item/Files/from.csv".into(),
                OneLakeProperties {
                    status_code: 200,
                    resource_type: Some("file".into()),
                    is_shortcut: false,
                },
            ),
        ],
    );
    let mut context = Context {
        resolver: PathResolver::new(
            &catalog,
            PathElement::from_fabric(tenant),
            HomeContext::Unspecified,
            "ls",
        )?,
        local_kind: match input["local_kind"].as_str().unwrap_or("file") {
            "file" => LocalKind::File,
            "directory" => LocalKind::Directory,
            _ => LocalKind::Other,
        },
    };
    let bytes = |value: &Value| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        value
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_u64()
                            .and_then(|byte| u8::try_from(byte).ok())
                            .ok_or_else(|| "invalid offline byte".into())
                    })
                    .collect()
            })
            .unwrap_or_else(|| Ok(Vec::new()))
    };
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
        replies.push_back(AsyncRawResponse::from_bytes(
            StatusCode::from(
                reply["status"]
                    .as_u64()
                    .and_then(|status| u16::try_from(status).ok())
                    .ok_or("invalid status")?,
            ),
            headers,
            bytes(&reply["body"])?,
        ));
    }
    let network = Arc::new(Network {
        replies: Mutex::new(replies),
        requests: Mutex::new(Vec::new()),
    });
    let executor = OneLakeHttpExecutor::new(Arc::new(HttpExecutor::new(
        network.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )?));
    let client = OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::OneLake.audience())?,
    );
    let mut files = Files {
        bytes: bytes(&input["bytes"])?,
        reads: 0,
    };
    let mut ui = Ui {
        confirmed: input["confirm"].as_bool().unwrap_or(true),
        notices: Vec::new(),
        prompts: Vec::new(),
    };
    let result = ResourceTransferHost {
        context: &mut context,
        storage: OneLakeWriteHost {
            client: &client,
            executor: &executor,
            ui: &mut ui,
            read_limits: OneLakeLimits::default(),
            write_limits: OneLakeWriteLimits::default(),
        },
        files: Some(&mut files),
    }
    .execute(&command, async {
        if input["cancelled"].as_bool().unwrap_or(false) {
            return;
        }
        pending().await
    })
    .await;
    let mut outputs = Vec::new();
    let mut plans = Vec::new();
    let mut evidence = Value::Null;
    let mut declined = false;
    let error = match result {
        Ok(result) => {
            declined = result.declined;
            evidence = json!({
                "status":result.outcome.map(|outcome|outcome.status()),
                "upload":result.upload.as_ref().map(|upload| json!({
                    "create":upload.create.status(), "append":upload.append.map(|value|value.status()),
                    "flush":upload.flush.map(|value|value.status()),
                })),
            });
            for output in result.outputs {
                outputs.push(output.message.clone());
                let plan = OutputEnvelope::new(Timestamp::parse("2000-01-01T00:00:00Z")?, output)
                    .render(Indent::Zero)?;
                plans.push(json!({"stream":if plan.stream == OutputStream::Stdout { "stdout" } else { "stderr" },
                    "exit":plan.exit_code,"payload":String::from_utf8(plan.payload)?}));
            }
            Value::Null
        }
        Err(error) => {
            if let OneLakeWriteError::Storage(storage) = &error {
                evidence = json!({
                    "mutation_may_have_occurred":storage.mutation_may_have_occurred,
                    "completed_steps":storage.completed_steps.len(),
                });
            }
            let mut output = error.output();
            let failure = json!({"code":output.error_code,"message":output.message});
            output.command = Some(command.name().into());
            let plan = OutputEnvelope::new(Timestamp::parse("2000-01-01T00:00:00Z")?, output)
                .render(Indent::Zero)?;
            plans.push(json!({"stream":if plan.stream == OutputStream::Stdout { "stdout" } else { "stderr" },
                "exit":plan.exit_code,"payload":String::from_utf8(plan.payload)?}));
            failure
        }
    };
    Ok(json!({
        "command":command.name(),"requests":*network.requests.lock().unwrap(),
        "remaining_replies":network.replies.lock().unwrap().len(),
        "notices":ui.notices,"prompts":ui.prompts,"outputs":outputs,
        "error":error,"evidence":evidence,"declined":declined,
        "plans":plans,"file_reads":files.reads,
    }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?).await?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> &'static str {
        "/Team.Workspace/Lake.Lakehouse/Files/from.csv"
    }
    fn target() -> &'static str {
        "/Team.Workspace/Lake.Lakehouse/Files/to.csv"
    }

    #[tokio::test]
    async fn copy_happy_path_preserves_real_read_create_append_flush_bytes_and_output() {
        let trace = probe(&json!({
            "argv":["cp",remote(),target()],
            "replies":[
                {"status":200,"body":[0,255,1],"headers":{"Content-Length":"3","Content-Type":"text/csv"}},
                {"status":201},{"status":202},{"status":200},
            ],
        })).await.unwrap();
        assert_eq!(trace["error"], Value::Null, "{trace}");
        let requests = trace["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0]["method"], "get");
        assert_eq!(requests[2]["body"], json!([0, 255, 1]));
        assert!(
            requests[3]["url"]
                .as_str()
                .unwrap()
                .ends_with("?action=flush&position=3")
        );
        assert_eq!(
            trace["evidence"]["upload"],
            json!({"create":201,"append":202,"flush":200})
        );
        assert_eq!(trace["outputs"], json!(["Done"]));
        assert_eq!(trace["plans"][0]["exit"], 0);
    }

    #[tokio::test]
    async fn move_happy_path_preserves_confirmation_rename_header_and_acceptance() {
        for status in [200, 202] {
            let trace = probe(&json!({
                "argv":["mv",remote(),target()],
                "confirm":true,"replies":[{"status":status}],
            }))
            .await
            .unwrap();
            assert_eq!(trace["error"], Value::Null);
            assert_eq!(trace["prompts"], json!(["Are you sure?"]));
            assert_eq!(
                trace["requests"][0]["headers"]["x-ms-rename-source"],
                "workspace/item/Files/from.csv"
            );
            assert_eq!(trace["evidence"]["status"], status);
            if status == 202 {
                assert_eq!(trace["outputs"], json!([]));
            }
        }
    }

    #[tokio::test]
    async fn local_copy_reads_only_injected_bytes_and_preserves_partial_upload_failure() {
        let trace = probe(&json!({
            "argv":["cp","local.csv",target()],"bytes":[1,2,3],
            "replies":[{"status":201},{"status":403}],
        }))
        .await
        .unwrap();
        assert_eq!(trace["file_reads"], 1);
        assert_eq!(trace["requests"].as_array().unwrap().len(), 2);
        assert_eq!(trace["error"]["code"], "Forbidden");
        assert_eq!(trace["evidence"]["mutation_may_have_occurred"], true);
        assert_eq!(trace["evidence"]["completed_steps"], 1);
        assert_eq!(trace["outputs"], json!([]));
        assert_eq!(trace["plans"][0]["stream"], "stdout");
        assert_eq!(trace["plans"][0]["exit"], 1);
    }

    #[tokio::test]
    async fn cancellation_decline_and_unsupported_options_never_dispatch_mutations() {
        for case in [
            json!({"argv":["mv",remote(),target()],"confirm":false,"replies":[]}),
            json!({"argv":["cp",remote(),target()],"cancelled":true,"replies":[]}),
            json!({"argv":["cp",remote(),target(),"--recursive"],"replies":[]}),
            json!({"argv":["cp",remote(),target(),"--block_on_path_collision"],"replies":[]}),
            json!({"argv":["cp","local.csv",target()],"local_kind":"directory","replies":[]}),
        ] {
            let trace = probe(&case).await.unwrap();
            assert_eq!(trace["requests"], json!([]), "{trace}");
            assert_eq!(trace["file_reads"], 0);
        }
    }
}
