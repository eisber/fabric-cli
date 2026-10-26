// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reusable, secret-free command consumer. One JSON input line produces one trace.
//! This executable has no production transport or credential provider.

use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CapacitySelection, CommandFuture, CreateOptions, MutationError, MutationHost, MutationMetadata,
    MutationUi, NoticeKind,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead},
    sync::Arc,
};

struct Operations {
    requests: Vec<Value>,
    response: ResourceOutcome,
}

impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        self.requests.push(json!({
            "method": match operation.method() {
                ResourceMethod::Get => "get", ResourceMethod::Post => "post",
                ResourceMethod::Patch => "patch", ResourceMethod::Delete => "delete",
            },
            "uri": operation.path().trim_start_matches("/v1/"),
            "payload": operation.body(),
            "wait": operation.completion() != CompletionPolicy::ReturnAccepted,
        }));
        Box::pin(std::future::ready(Ok(self.response.clone())))
    }
}

struct Ui(bool);

impl MutationUi for Ui {
    fn notice(&mut self, _: NoticeKind, _: &str) {}
    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        Box::pin(std::future::ready(self.0))
    }
}

struct Metadata(Value);

impl MutationMetadata for Metadata {
    fn read<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Value, MutationError>> {
        Box::pin(std::future::ready(Ok(self.0.clone())))
    }
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let id = input.get("id").and_then(Value::as_str).map(str::to_owned);
    let kind = input["kind"].as_str().ok_or("missing kind")?;
    let tenant = Element::tenant("Tenant", "tenant");
    let workspace = Element::child(
        "Team",
        if kind == "Workspace" {
            id.clone()
        } else {
            Some("ws".into())
        },
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant,
    )?;
    let element = match kind {
        "Workspace" => workspace,
        "Folder" => Element::child("Thing", id, NodeKind::Folder, workspace)?,
        kind => Element::child(
            "Thing",
            id,
            NodeKind::Item(ItemType::parse(kind)?),
            workspace,
        )?,
    };
    let mut operations = Operations {
        requests: Vec::new(),
        response: if input["status"] == 202 {
            ResourceOutcome::Accepted {
                operation_id: Some("op".into()),
            }
        } else {
            ResourceOutcome::Completed {
                status: input["status"]
                    .as_u64()
                    .and_then(|s| u16::try_from(s).ok())
                    .unwrap_or(200),
                data: input.get("data").cloned(),
            }
        },
    };
    let mut ui = Ui(input["confirm"].as_bool().unwrap_or(true));
    let mut metadata = Metadata(input["metadata"].clone());
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    let result = match input["command"].as_str().ok_or("missing command")? {
        "create" => {
            host.create(
                &element,
                &CreateOptions {
                    params: input["params"].as_object().cloned().unwrap_or_default(),
                    default_capacity_id: Some("capacity".into()),
                    capacity: if input["unassigned"] == true {
                        CapacitySelection::Unassigned
                    } else {
                        CapacitySelection::Default
                    },
                },
            )
            .await
        }
        "remove" => {
            host.remove_with_options(
                &element,
                input["force"].as_bool().unwrap_or(false),
                input["hard"].as_bool().unwrap_or(false),
            )
            .await
        }
        "set" => {
            host.set(
                &element,
                input["query"].as_str().ok_or("missing query")?,
                input["input"].as_str().ok_or("missing input")?,
                input["force"].as_bool().unwrap_or(false),
            )
            .await
        }
        "exists" => MutationHost::exists(&element),
        "start" => {
            host.start(&element, input["force"].as_bool().unwrap_or(false))
                .await
        }
        "stop" => {
            host.stop(&element, input["force"].as_bool().unwrap_or(false))
                .await
        }
        "assign" => {
            host.assign_capacity(
                &element,
                input["capacity_id"].as_str().ok_or("missing capacity")?,
                input["capacity_name"]
                    .as_str()
                    .ok_or("missing capacity name")?,
                input["force"].as_bool().unwrap_or(false),
            )
            .await
        }
        "unassign" => {
            host.unassign_capacity(
                &element,
                input["capacity_id"].as_str().ok_or("missing capacity")?,
                input["capacity_name"]
                    .as_str()
                    .ok_or("missing capacity name")?,
                input["force"].as_bool().unwrap_or(false),
            )
            .await
        }
        _ => return Err("unsupported probe command".into()),
    };
    Ok(match result {
        Ok(result) => json!({
            "requests": operations.requests,
            "outputs": result.outputs.iter().map(|output| json!({
                "message": output.message, "data": output.data,
            })).collect::<Vec<_>>(),
            "error": null,
        }),
        Err(error) => json!({"requests": operations.requests, "outputs": [], "error": error.code}),
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line?)?;
        println!("{}", probe(&input).await?);
    }
    Ok(())
}
