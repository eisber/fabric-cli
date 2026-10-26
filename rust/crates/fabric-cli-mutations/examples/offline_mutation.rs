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
    collections::VecDeque,
    io::{self, BufRead},
    sync::Arc,
};

struct Operations {
    requests: Vec<Value>,
    response: ResourceOutcome,
    scripted: Option<VecDeque<Result<ResourceOutcome, ResourceError>>>,
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
        let result = match &mut self.scripted {
            Some(responses) => responses
                .pop_front()
                .expect("unexpected create/dependency request"),
            None => Ok(self.response.clone()),
        };
        Box::pin(std::future::ready(result))
    }
}

struct Ui {
    confirmed: bool,
    notices: Vec<Value>,
}

impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, text: &str) {
        self.notices.push(json!({"kind":match kind { NoticeKind::Progress=>"progress",NoticeKind::Warning=>"warning",NoticeKind::Help=>"help" },"message":text}));
    }
    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        Box::pin(std::future::ready(self.confirmed))
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
    let owning_workspace = Arc::clone(&workspace);
    let source_parent = if input["source_parent"]
        .as_str()
        .is_some_and(|parent| parent != "root")
    {
        let id = input["source_parent"]
            .as_str()
            .ok_or("invalid source parent")?;
        Element::child(
            id.to_uppercase(),
            Some(id.into()),
            NodeKind::Folder,
            Arc::clone(&workspace),
        )?
    } else {
        Arc::clone(&workspace)
    };
    let element = match kind {
        "Workspace" => workspace,
        "Folder" => Element::child("Thing", id, NodeKind::Folder, source_parent)?,
        kind => Element::child(
            "Thing",
            id,
            NodeKind::Item(ItemType::parse(kind)?),
            source_parent,
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
        scripted: input["responses"].as_array().map(|responses| {
            responses
                .iter()
                .map(|response| {
                    if let Some(code) = response["error"].as_str() {
                        let mut error = ResourceError::input();
                        error.code = code.into();
                        error.message = response["message"]
                            .as_str()
                            .unwrap_or("synthetic failure")
                            .into();
                        error.status = response["status"]
                            .as_u64()
                            .and_then(|status| u16::try_from(status).ok());
                        error.mutation_may_have_occurred = true;
                        return Err(error);
                    }
                    Ok(if response["status"] == 202 {
                        ResourceOutcome::Accepted { operation_id: None }
                    } else {
                        ResourceOutcome::Completed {
                            status: response["status"]
                                .as_u64()
                                .and_then(|status| u16::try_from(status).ok())
                                .unwrap_or(200),
                            data: response.get("data").cloned(),
                        }
                    })
                })
                .collect()
        }),
    };
    let mut ui = Ui {
        confirmed: input["confirm"].as_bool().unwrap_or(true),
        notices: Vec::new(),
    };
    let mut metadata = Metadata(input["metadata"].clone());
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    let mut moved_count = None;
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
        "move_folder" => {
            let parent = match input["destination"].as_str().unwrap_or("root") {
                "root" => owning_workspace,
                id => Element::child(
                    id.to_uppercase(),
                    Some(id.into()),
                    NodeKind::Folder,
                    owning_workspace,
                )?,
            };
            host.move_folder(&element, &parent).await.map(|result| {
                moved_count = Some(result.moved_count);
                result.result
            })
        }
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
    let creation = match &result {
        Ok(result) => result.creation.as_deref(),
        Err(error) => error.creation.as_deref(),
    };
    let cache=creation.map(|creation|creation.effects.iter().map(|effect|match effect {
        fabric_cli_mutations::CacheEffect::Created {target,..}=>json!({"action":"upsert","name":target.name()}),
        fabric_cli_mutations::CacheEffect::Invalidate {target}=>json!({"action":"invalidate","workspace":target.workspace().and_then(|workspace|workspace.id().map(str::to_owned))}),
        _=>panic!("unexpected create cache effect"),
    }).collect::<Vec<_>>()).unwrap_or_default();
    let mut output = match result {
        Ok(result) => json!({
            "requests": operations.requests,
            "outputs": result.outputs.iter().map(|output| json!({
                "message": output.message, "data": output.data,
            })).collect::<Vec<_>>(),
            "error": null,
        }),
        Err(error) => {
            let mut output =
                json!({"requests":operations.requests,"outputs":[],"error":error.code});
            if input["capture_error"] == true {
                output["error_message"] = json!(error.output().message);
            }
            output
        }
    };
    if let Some(count) = moved_count {
        output["moved_count"] = count.into();
    }
    if input["capture_creation"] == true {
        output["cache"] = Value::Array(cache);
        output["notices"] = Value::Array(ui.notices);
    }
    Ok(output)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line?)?;
        println!("{}", probe(&input).await?);
    }
    Ok(())
}
