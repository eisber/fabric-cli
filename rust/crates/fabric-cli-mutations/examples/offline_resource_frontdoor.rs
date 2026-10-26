// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic argv -> family parser -> real handler/SDK/session/shared HTTP proof.
//! All credentials, HTTP responses, time, metadata, configuration and UI are injected.

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use clap::Command;
use fabric_cli_context::{
    Catalog, Element, HierarchyCatalog, HomeContext, ItemType, NodeKind, OneLakeProperties,
    PathElement, PathInput, PathKind, PathResolver, ResolveOptions, VirtualWorkspaceKind,
    WorkspaceType,
};
use fabric_cli_mutations::{
    CacheEffect, CommandFuture, FabricMutationBackend, MutationError, MutationHost,
    MutationMetadata, MutationUi, NoticeKind,
    frontdoor::{
        ResourceConfig, ResourceContext, ResourceHost, command_from_matches, resource_subcommands,
    },
};
use fabric_cli_output::{Indent, OutputEnvelope, OutputStream, Timestamp};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor, PublicOrigin,
};
use fabric_sdk_operations::OperationLimits;
use fabric_sdk_workspaces::{CredentialSelection, mutations::ResourceOutcome};
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
            "offline-resource-frontdoor",
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
        let body = Bytes::from(request.body());
        self.requests.lock().unwrap().push(json!({
            "method": match request.method() {
                Method::Get => "get", Method::Post => "post",
                Method::Patch => "patch", Method::Delete => "delete",
                _ => panic!("unexpected resource method"),
            },
            "url": request.url().as_str(),
            "payload": if body.is_empty() { Value::Null } else {
                serde_json::from_slice::<Value>(&body).expect("invalid request body")
            },
        }));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected mutation or replay"))
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
    prompts: Vec<String>,
    notices: Vec<Value>,
}

impl MutationUi for Ui {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        self.notices.push(json!({
            "kind": match kind {
                NoticeKind::Progress => "progress",
                NoticeKind::Warning => "warning",
                NoticeKind::Help => "help",
            },
            "message": message,
        }));
    }

    fn confirm<'a>(&'a mut self, message: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(message.into());
        Box::pin(std::future::ready(self.confirmed))
    }
}

struct Metadata {
    value: Value,
    reads: usize,
}

impl MutationMetadata for Metadata {
    fn read<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Value, MutationError>> {
        self.reads += 1;
        Box::pin(std::future::ready(Ok(self.value.clone())))
    }
}

struct Context<'a> {
    resolver: PathResolver<'a, HierarchyCatalog>,
    resolutions: Vec<Value>,
}

impl ResourceContext for Context<'_> {
    fn resolve(
        &mut self,
        tokens: &[String],
        options: ResolveOptions,
    ) -> Result<Arc<PathElement>, MutationError> {
        self.resolutions.push(json!({
            "path": tokens, "raise_error": options.raise_error,
            "supports_local_path": options.supports_local_path,
        }));
        let tokens: Vec<_> = tokens.iter().map(String::as_str).collect();
        self.resolver
            .resolve(PathInput::Tokens(&tokens), options)
            .map(|resolution| {
                assert!(resolution.notices.is_empty());
                resolution.element
            })
            .map_err(|failure| MutationError::new(&failure.error.code, failure.error.message))
    }
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::iter::once(Ok("fab-rs".to_owned()))
        .chain(
            input["argv"]
                .as_array()
                .ok_or("missing argv")?
                .iter()
                .map(|token| token.as_str().map(str::to_owned).ok_or("invalid argv")),
        )
        .collect::<Result<_, _>>()?;
    let matches = Command::new("fab-rs")
        .subcommands(resource_subcommands())
        .try_get_matches_from(arguments)?;
    let command = command_from_matches(&matches)?;
    let kind = input["kind"].as_str().ok_or("missing fixture kind")?;
    let tenant = Element::tenant("Tenant", "tenant");
    let workspace = Element::child(
        "Team",
        if kind == "Workspace" {
            input["id"].as_str().map(str::to_owned)
        } else {
            Some("ws".into())
        },
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant.clone(),
    )?;
    let mut elements = vec![workspace.clone()];
    if kind != "Workspace" && kind != "Capacity" {
        elements.push(Element::child(
            "Thing",
            input["id"].as_str().map(str::to_owned),
            if kind == "Folder" {
                NodeKind::Folder
            } else {
                NodeKind::Item(ItemType::parse(kind)?)
            },
            workspace,
        )?);
    }
    let capacities = PathElement::child(
        ".capacities", None,
        PathKind::VirtualWorkspace(VirtualWorkspaceKind::Capacity),
        PathElement::from_fabric(tenant.clone()),
    )?;
    let capacity = PathElement::child(
        "Chosen", Some("cap".into()),
        PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Capacity),
        capacities,
    )?;
    let catalog = HierarchyCatalog::new(
        Catalog::new(elements),
        vec![capacity],
        vec![(
            "ws/id/Files".into(),
            OneLakeProperties {
                status_code: 200,
                resource_type: Some("directory".into()),
                is_shortcut: false,
            },
        )],
    );
    let mut context = Context {
        resolver: PathResolver::new(
            &catalog,
            PathElement::from_fabric(tenant),
            HomeContext::Unspecified,
            "ls",
        )?,
        resolutions: Vec::new(),
    };
    let replies = input["replies"]
        .as_array()
        .ok_or("missing offline replies")?
        .iter()
        .map(|reply| {
            Ok(AsyncRawResponse::from_bytes(
                StatusCode::from(
                    reply["status"]
                        .as_u64()
                        .and_then(|value| u16::try_from(value).ok())
                        .ok_or("invalid offline status")?,
                ),
                Headers::new(),
                serde_json::to_vec(&reply["data"])?,
            ))
        })
        .collect::<Result<VecDeque<_>, Box<dyn std::error::Error>>>()?;
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
        async {
            if input["cancelled"].as_bool().unwrap_or(false) {
                return;
            }
            pending().await
        },
    );
    let mut ui = Ui {
        confirmed: input["confirm"].as_bool().unwrap_or(true),
        prompts: Vec::new(),
        notices: Vec::new(),
    };
    let mut metadata = Metadata {
        value: input["metadata"].clone(),
        reads: 0,
    };
    let result = ResourceHost {
        context: &mut context,
        mutations: MutationHost {
            operations: &mut backend,
            ui: &mut ui,
            metadata: &mut metadata,
        },
        config: ResourceConfig {
            default_capacity_id: Some("capacity".into()),
        },
    }
    .execute(&command)
    .await;
    let mut outputs = Vec::new();
    let mut plans = Vec::new();
    let mut effects = Vec::new();
    let mut declined = false;
    let mut outcome = Value::Null;
    let mut operation = Value::Null;
    let error = match result {
        Ok(result) => {
            declined = result.declined;
            outcome = match result.outcome {
                None => Value::Null,
                Some(ResourceOutcome::Accepted { .. }) => json!("accepted"),
                Some(ResourceOutcome::Completed { .. }) => json!("completed"),
            };
            for effect in result.effects {
                effects.push(match effect {
                    CacheEffect::Created { .. } => json!("created"),
                    CacheEffect::Invalidate { .. } => json!("invalidate"),
                    CacheEffect::Deleted { .. } => json!("deleted"),
                    CacheEffect::Renamed { name, .. } => json!({"renamed":name}),
                    CacheEffect::MovedFolder { .. } => json!("moved"),
                });
            }
            for output in result.outputs {
                outputs.push(json!({"message":output.message,"data":output.data}));
                let envelope = OutputEnvelope::new(
                    Timestamp::parse("2000-01-01T00:00:00Z")?,
                    output,
                );
                let plan = envelope.render(Indent::Zero)?;
                plans.push(json!({
                    "stream": if plan.stream == OutputStream::Stdout { "stdout" } else { "stderr" },
                    "exit": plan.exit_code,
                    "payload": String::from_utf8(plan.payload)?,
                }));
            }
            Value::Null
        }
        Err(error) => {
            operation = error.operation.as_ref().map_or(Value::Null, |operation| json!({
                "id": operation.operation_id,
                "mutation_may_have_occurred": operation.mutation_may_have_occurred,
            }));
            let mut output = error.output();
            output.command = Some(command.name().into());
            let plan = OutputEnvelope::new(
                Timestamp::parse("2000-01-01T00:00:00Z")?,
                output,
            )
            .render(Indent::Zero)?;
            plans.push(json!({
                "stream": if plan.stream == OutputStream::Stdout { "stdout" } else { "stderr" },
                "exit": plan.exit_code,
                "payload": String::from_utf8(plan.payload)?,
            }));
            json!({"code":error.code,"message":error.message})
        }
    };
    Ok(json!({
        "command": command.name(),
        "output_format": command.args().output_format,
        "requests": *network.requests.lock().unwrap(),
        "remaining_replies": network.replies.lock().unwrap().len(),
        "outputs": outputs, "plans": plans,
        "effects": effects, "declined": declined, "outcome": outcome,
        "error": error, "operation": operation,
        "notices": ui.notices, "prompts": ui.prompts,
        "metadata_reads": metadata.reads, "resolutions": context.resolutions,
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
    use fabric_cli_mutations::frontdoor::{ResourceCommand, creation_parameters};

    #[tokio::test]
    async fn five_canonical_leaves_execute_real_handlers_sdk_and_http() {
        let cases = [
            json!({"argv":["exists","Team.Workspace"],"kind":"Workspace","id":"ws","replies":[]}),
            json!({"argv":["mkdir","Team.Workspace/Thing.Folder"],"kind":"Folder","replies":[{"status":201,"data":{"id":"created"}}]}),
            json!({"argv":["rm","Team.Workspace/Thing.Notebook","-f","--hard"],"kind":"Notebook","id":"id","replies":[{"status":200}]}),
            json!({"argv":["set","Team.Workspace/Thing.Notebook","-q","displayName","-i","After","-f"],"kind":"Notebook","id":"id","metadata":{"displayName":"Thing"},"replies":[{"status":200}]}),
            json!({"argv":["ln","Team.Workspace/Thing.Lakehouse/Files/Chosen.Shortcut","--type","oneLake","-i","{'workspaceId':'other','itemId':'target','path':'Files'}","-f"],"kind":"Lakehouse","id":"id","replies":[{"status":201,"data":{"name":"Chosen"}}]}),
        ];
        let expected = [
            ("exists", "", Value::Null, json!([])),
            ("mkdir", "/v1/workspaces/ws/folders", json!({"displayName":"Thing"}), json!(["created"])),
            ("rm", "/v1/workspaces/ws/items/id?hardDelete=true", Value::Null, json!(["deleted"])),
            ("set", "/v1/workspaces/ws/notebooks/id", json!({"displayName":"After"}), json!([{"renamed":"After"}])),
            ("ln", "/v1/workspaces/ws/items/id/shortcuts?shortcutConflictPolicy=GenerateUniqueName", json!({"path":"Files","name":"Chosen","target":{"oneLake":{"workspaceId":"other","itemId":"target","path":"Files"}}}), json!([])),
        ];
        for (case, (leaf, path, body, effects)) in cases.iter().zip(expected) {
            let trace = probe(case).await.unwrap();
            assert_eq!(trace["command"], leaf);
            assert_eq!(trace["error"], Value::Null, "{trace}");
            assert_eq!(trace["remaining_replies"], 0);
            assert_eq!(trace["effects"], effects);
            if path.is_empty() {
                assert_eq!(trace["requests"], json!([]));
                assert_eq!(trace["outputs"][0]["message"], "true");
            } else {
                assert_eq!(trace["requests"].as_array().unwrap().len(), 1);
                assert_eq!(trace["requests"][0]["url"], format!("https://api.fabric.microsoft.com{path}"));
                assert_eq!(trace["requests"][0]["payload"], body);
                assert_eq!(trace["outcome"], "completed");
            }
            assert_eq!(trace["plans"][0]["stream"], "stdout");
            assert_eq!(trace["plans"][0]["exit"], 0);
        }
    }

    #[tokio::test]
    async fn accepted_delete_retains_acceptance_and_cache_without_claiming_completion() {
        let trace = probe(&json!({
            "argv":["rm","Team.Workspace/Thing.Notebook","-f"],
            "kind":"Notebook","id":"id","replies":[{"status":202}],
        })).await.unwrap();
        assert_eq!(trace["outcome"], "accepted");
        assert_eq!(trace["outputs"][0]["message"], "'Thing.Notebook' is being deleted");
        assert_eq!(trace["effects"], json!(["deleted"]));
        assert_eq!(trace["requests"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn decline_preserves_source_read_order_and_sends_no_mutation() {
        for kind in ["Workspace", "Folder", "Notebook"] {
            let path = if kind == "Workspace" {
                "Team.Workspace".into()
            } else {
                format!("Team.Workspace/Thing.{kind}")
            };
            let trace = probe(&json!({
                "argv":["set",path,"-q","displayName","-i","After"],
                "kind":kind,"id":"id","metadata":{"displayName":"before"},
                "confirm":false,"replies":[],
            })).await.unwrap();
            assert_eq!(trace["error"], Value::Null);
            assert_eq!(trace["declined"], true);
            assert_eq!(trace["requests"], json!([]));
            assert_eq!(trace["effects"], json!([]));
            assert_eq!(trace["metadata_reads"], usize::from(kind == "Workspace"));
            assert_eq!(trace["prompts"], json!(["Are you sure?"]));
        }
    }

    #[tokio::test]
    async fn explicit_gaps_and_lookup_failures_do_not_become_success_or_absence() {
        for case in [
            json!({"argv":["rm","Team.Workspace","-f"],"kind":"Workspace","id":"ws","replies":[]}),
            json!({"argv":["set","Team.Workspace/Thing.Notebook","-q","definition","-i","{}","-f"],"kind":"Notebook","id":"id","replies":[]}),
            json!({"argv":["exists","Team.Workspace","--skill","skill"],"kind":"Workspace","id":"ws","replies":[]}),
            json!({"argv":["ln","Team.Workspace/Thing.Lakehouse/Files/Chosen.Shortcut","--type","oneLake","--target","other"],"kind":"Lakehouse","id":"id","replies":[]}),
        ] {
            let trace = probe(&case).await.unwrap();
            assert_eq!(trace["error"]["code"], "NotSupported", "{trace}");
            assert_eq!(trace["requests"], json!([]));
            assert_eq!(trace["plans"][0]["exit"], 1);
        }

        #[tokio::test]
        async fn cancellation_and_service_failures_preserve_error_exits_without_replay() {
            let cancelled = probe(&json!({
                "argv":["rm","Team.Workspace/Thing.Notebook","-f"],
                "kind":"Notebook","id":"id","cancelled":true,"replies":[],
            })).await.unwrap();
            assert_eq!(cancelled["error"]["code"], "LongRunningOperationCancelled");
            assert_eq!(cancelled["requests"], json!([]));
            assert_eq!(cancelled["plans"][0]["exit"], 1);
            for (status, code, exit) in [(401, "Unauthorized", 4), (403, "Forbidden", 1)] {
                let trace = probe(&json!({
                    "argv":["rm","Team.Workspace/Thing.Notebook","-f"],
                    "kind":"Notebook","id":"id",
                    "replies":[{"status":status,"data":{"error":{"code":code,"message":"synthetic failure"}}}],
                })).await.unwrap();
                assert_eq!(trace["error"]["code"], code, "{trace}");
                assert_eq!(trace["plans"][0]["exit"], exit);
                assert_eq!(trace["requests"].as_array().unwrap().len(), 1);
                assert_eq!(trace["outputs"], json!([]));
                assert_eq!(trace["effects"], json!([]));
            }

            #[tokio::test]
            async fn lifecycle_and_assignment_leaves_use_released_sdk_routes_and_ordering() {
                for (verb, state, action) in [
                    ("start", "Stopped", "startMirroring"),
                    ("stop", "Running", "stopMirroring"),
                ] {
                    let trace = probe(&json!({
                        "argv":[verb,"Team.Workspace/Thing.MirroredDatabase","-f"],
                        "kind":"MirroredDatabase","id":"id",
                        "replies":[{"status":200,"data":{"status":state}},{"status":200}],
                    })).await.unwrap();
                    assert_eq!(trace["error"], Value::Null, "{trace}");
                    assert_eq!(trace["requests"].as_array().unwrap().len(), 2);
                    assert_eq!(trace["requests"][0]["url"], "https://api.fabric.microsoft.com/v1/workspaces/ws/mirroredDatabases/id/getMirroringStatus");
                    assert_eq!(trace["requests"][1]["url"], format!("https://api.fabric.microsoft.com/v1/workspaces/ws/mirroredDatabases/id/{action}"));
                    assert_eq!(trace["requests"][1]["payload"], Value::Null);
                }
                for verb in ["assign", "unassign"] {
                    let replies = if verb == "assign" {
                        json!([{"status":200}])
                    } else {
                        json!([{"status":200,"data":{"capacityId":"cap"}},{"status":200}])
                    };
                    let trace = probe(&json!({
                        "argv":[verb,"/.capacities/Chosen.Capacity","-W","Team.Workspace","-f"],
                        "kind":"Capacity","id":"cap","replies":replies,
                    })).await.unwrap();
                    assert_eq!(trace["error"], Value::Null, "{trace}");
                    let requests = trace["requests"].as_array().unwrap();
                    let final_request = requests.last().unwrap();
                    assert_eq!(final_request["url"], format!("https://api.fabric.microsoft.com/v1/workspaces/ws/{}",
                        if verb == "assign" { "assignToCapacity" } else { "unassignFromCapacity" }));
                    assert_eq!(final_request["payload"], if verb == "assign" { json!({"capacityId":"cap"}) } else { Value::Null });
                    assert_eq!(trace["resolutions"].as_array().unwrap().len(), 2);
                    assert_eq!(trace["metadata_reads"], 0);
                }
            }

            #[tokio::test]
            async fn force_does_not_skip_mirroring_state_or_workspace_linkage_checks() {
                let state = probe(&json!({
                    "argv":["start","Team.Workspace/Thing.MirroredDatabase","-f"],
                    "kind":"MirroredDatabase","id":"id",
                    "replies":[{"status":200,"data":{"status":"Running"}}],
                })).await.unwrap();
                assert_eq!(state["error"]["code"], "AlreadyRunning");
                assert_eq!(state["requests"].as_array().unwrap().len(), 1);
                let linkage = probe(&json!({
                    "argv":["unassign","/.capacities/Chosen.Capacity","-W","Team.Workspace","-f"],
                    "kind":"Capacity","id":"cap",
                    "replies":[{"status":200,"data":{"capacityId":"other"}}],
                })).await.unwrap();
                assert_eq!(linkage["error"]["code"], "InvalidInput");
                assert_eq!(linkage["requests"].as_array().unwrap().len(), 1);
                assert_eq!(linkage["prompts"], json!([]));
            }
        }
        let missing = probe(&json!({"argv":["rm","Team.Workspace/Thing.Notebook","-f"],"kind":"Notebook","replies":[]})).await.unwrap();
        assert_eq!(missing["error"]["code"], "NotFound");
        assert_eq!(missing["requests"], json!([]));
    }

    #[test]
    fn source_aliases_defaults_and_scalar_versus_variadic_arguments_are_retained() {
        let parse = |tokens: Vec<&str>| {
            command_from_matches(&Command::new("fab-rs")
                .subcommands(resource_subcommands())
                .try_get_matches_from(tokens).unwrap()).unwrap()
        };
        let ResourceCommand::Mkdir { params, .. } = parse(vec!["fab-rs", "create", "x.Notebook"]) else { panic!("mkdir alias") };
        assert_eq!(params, ["run=true"]);
        let ResourceCommand::Mkdir { params, .. } = parse(vec!["fab-rs", "mkdir", "x.Notebook", "-P"]) else { panic!("mkdir") };
        assert!(params.is_empty());
        let ResourceCommand::Set { query, input, .. } = parse(vec!["fab-rs", "set", "x", "-q", "'displayName'", "-i", "'one", "two'"]) else { panic!("set") };
        assert_eq!(query, "'displayName'");
        assert_eq!(fabric_cli_query::join_query_args(&input), "one two");
        for tokens in [
            vec!["fab-rs","exists","x","--force"],
            vec!["fab-rs","mkdir","x","--capacity","capacity"],
            vec!["fab-rs","ln","x","--type","onelake","-i","{}"],
        ] {
            assert!(Command::new("fab-rs").subcommands(resource_subcommands()).try_get_matches_from(tokens).is_err());
        }
    }

    #[test]
    fn creation_values_do_not_implicitly_turn_strings_into_json_scalars() {
        assert_eq!(
            creation_parameters(&["run=true,n=42,nested.value='true',rows=[{\"Keep\":true}]".into()]).unwrap(),
            json!({"run":"true","n":"42","nested":{"value":"'true'"},"rows":[{"Keep":true}]}).as_object().unwrap().clone(),
        );
        assert!(creation_parameters(&["not-a-key=value".into()]).is_err());
    }
}
