// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive contracts for fallible caller-owned input; no terminal or service.

#![deny(unnameable_test_items)]

use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CommandFuture, MutationError, MutationHost, MutationMetadata, MutationResult, MutationUi,
    NoticeKind, OneLakeShortcutHost, RemovalCatalog, RemovalHost, RemovalSort, RemovalUi,
    ShortcutCreateDestination, ShortcutCreateOptions,
};
use fabric_sdk_onelake::shortcuts::ShortcutPath;
use fabric_sdk_workspaces::mutations::{
    ResourceError, ResourceMethod, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};

#[path = "fallible_ui/storage.rs"]
mod storage;

fn input_error() -> MutationError {
    MutationError::new("ConfirmationInputFailed", "scripted input failure")
}

struct Ui {
    answer: Result<bool, MutationError>,
    selected: Result<Vec<String>, MutationError>,
    questions: Vec<String>,
    selections: Vec<String>,
    notices: Vec<String>,
}

impl Ui {
    fn new(answer: Result<bool, MutationError>) -> Self {
        Self {
            answer,
            selected: Ok(Vec::new()),
            questions: Vec::new(),
            selections: Vec::new(),
            notices: Vec::new(),
        }
    }
}

impl MutationUi for Ui {
    fn notice(&mut self, _: NoticeKind, message: &str) {
        self.notices.push(message.into());
    }

    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        panic!("production used the infallible confirmation boundary")
    }

    fn confirm_result<'a>(
        &'a mut self,
        message: &'a str,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        self.questions.push(message.into());
        Box::pin(std::future::ready(self.answer.clone()))
    }
}

impl RemovalUi for Ui {
    fn select<'a>(&'a mut self, _: &'a str, _: &'a [String]) -> CommandFuture<'a, Vec<String>> {
        panic!("production used the infallible selection boundary")
    }

    fn select_result<'a>(
        &'a mut self,
        message: &'a str,
        _: &'a [String],
    ) -> CommandFuture<'a, Result<Vec<String>, MutationError>> {
        self.selections.push(message.into());
        Box::pin(std::future::ready(self.selected.clone()))
    }
}

#[derive(Default)]
struct Operations {
    requests: Vec<(ResourceMethod, String)>,
    responses: VecDeque<(ResourceMethod, ResourceOutcome)>,
}

impl Operations {
    fn reply(&mut self, method: ResourceMethod, data: Option<Value>) {
        self.responses
            .push_back((method, ResourceOutcome::Completed { status: 200, data }));
    }
}

impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        self.requests
            .push((operation.method(), operation.path().into()));
        let (expected, response) = self.responses.pop_front().expect("unexpected operation");
        assert_eq!(operation.method(), expected);
        Box::pin(std::future::ready(Ok(response)))
    }
}

#[derive(Default)]
struct Metadata {
    reads: usize,
}

impl MutationMetadata for Metadata {
    fn read<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Value, MutationError>> {
        self.reads += 1;
        Box::pin(std::future::ready(Ok(json!({"displayName":"old"}))))
    }
}

fn tenant() -> Arc<Element> {
    Element::tenant("Tenant", "tenant")
}

fn workspace() -> Arc<Element> {
    Element::child(
        "Team",
        Some("ws".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant(),
    )
    .unwrap()
}

fn item(kind: &str) -> Arc<Element> {
    Element::child(
        "Book",
        Some("item".into()),
        NodeKind::Item(ItemType::parse(kind).unwrap()),
        workspace(),
    )
    .unwrap()
}

fn check_decline_or_failure(result: Result<MutationResult, MutationError>, failed: bool) {
    if failed {
        assert_eq!(result.unwrap_err(), input_error());
    } else {
        let result = result.unwrap();
        assert!(result.declined);
        assert!(result.outputs.is_empty());
        assert!(result.effects.is_empty());
        assert!(result.outcome.is_none());
    }
}

#[tokio::test]
async fn default_result_methods_preserve_legacy_adapters_without_requiring_new_methods() {
    struct Legacy;
    impl MutationUi for Legacy {
        fn notice(&mut self, _: NoticeKind, _: &str) {}
        fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
            Box::pin(std::future::ready(false))
        }
    }
    impl RemovalUi for Legacy {
        fn select<'a>(
            &'a mut self,
            _: &'a str,
            names: &'a [String],
        ) -> CommandFuture<'a, Vec<String>> {
            Box::pin(async move { names.to_vec() })
        }
    }
    let mut legacy = Legacy;
    assert!(!legacy.confirm_result("Are you sure?").await.unwrap());
    let names = vec!["Book.Notebook".into()];
    assert_eq!(
        legacy.select_result("Select items:", &names).await.unwrap(),
        names
    );
}

#[tokio::test]
async fn single_remove_errors_are_not_successful_declines_and_force_bypasses_input() {
    for force in [false, true] {
        for failed in [false, true] {
            let mut ui = Ui::new(if failed {
                Err(input_error())
            } else {
                Ok(false)
            });
            let mut operations = Operations::default();
            if force {
                operations.reply(ResourceMethod::Delete, None);
            }
            let mut metadata = Metadata::default();
            let result = MutationHost {
                operations: &mut operations,
                ui: &mut ui,
                metadata: &mut metadata,
            }
            .remove_with_options(&item("Notebook"), force, true)
            .await;
            if force {
                assert!(!result.unwrap().declined);
                assert!(ui.questions.is_empty());
                assert_eq!(operations.requests.len(), 1);
            } else {
                check_decline_or_failure(result, failed);
                assert_eq!(
                    ui.questions,
                    ["'Book.Notebook' will be deleted forever. Are you sure you want to proceed?"]
                );
                assert!(operations.requests.is_empty());
                assert_eq!(ui.notices.is_empty(), failed);
            }
        }
    }
}

#[tokio::test]
async fn set_retains_workspace_read_before_prompt_and_item_read_after_consent() {
    for is_workspace in [false, true] {
        for force in [false, true] {
            for failed in [false, true] {
                let target = if is_workspace {
                    workspace()
                } else {
                    item("Notebook")
                };
                let mut ui = Ui::new(if failed {
                    Err(input_error())
                } else {
                    Ok(false)
                });
                let mut operations = Operations::default();
                if force {
                    operations.reply(ResourceMethod::Patch, None);
                }
                let mut metadata = Metadata::default();
                let result = MutationHost {
                    operations: &mut operations,
                    ui: &mut ui,
                    metadata: &mut metadata,
                }
                .set(&target, "displayName", "new", force)
                .await;
                assert_eq!(metadata.reads, usize::from(is_workspace || force));
                assert_eq!(operations.requests.len(), usize::from(force));
                assert_eq!(ui.questions.len(), usize::from(!force));
                assert_eq!(
                    ui.notices[0],
                    "Modifying properties may lead to unintended consequences"
                );
                if force {
                    assert!(!result.unwrap().declined);
                } else {
                    check_decline_or_failure(result, failed);
                    assert_eq!(ui.notices.len(), 1);
                }
            }
        }
    }
}

#[tokio::test]
async fn lifecycle_keeps_preconfirmation_reads_but_never_dispatches_after_prompt_failure() {
    for action in ["start", "stop", "assign", "unassign"] {
        for force in [false, true] {
            for failed in [false, true] {
                let mut ui = Ui::new(if failed {
                    Err(input_error())
                } else {
                    Ok(false)
                });
                let mut operations = Operations::default();
                let before = usize::from(action != "assign");
                match action {
                    "start" | "stop" => operations.reply(
                        ResourceMethod::Post,
                        Some(json!({"status":if action == "start" {"Stopped"} else {"Running"}})),
                    ),
                    "unassign" => operations
                        .reply(ResourceMethod::Get, Some(json!({"capacityId":"capacity"}))),
                    _ => {}
                }
                if force {
                    operations.reply(ResourceMethod::Post, None);
                }
                let mut metadata = Metadata::default();
                let mut host = MutationHost {
                    operations: &mut operations,
                    ui: &mut ui,
                    metadata: &mut metadata,
                };
                let result = match action {
                    "start" => host.start(&item("MirroredDatabase"), force).await,
                    "stop" => host.stop(&item("MirroredDatabase"), force).await,
                    "assign" => {
                        host.assign_capacity(&workspace(), "capacity", "Cap.Capacity", force)
                            .await
                    }
                    _ => {
                        host.unassign_capacity(&workspace(), "capacity", "Cap.Capacity", force)
                            .await
                    }
                };
                assert_eq!(
                    operations.requests.len(),
                    before + usize::from(force),
                    "{action}"
                );
                assert!(operations.responses.is_empty());
                assert_eq!(ui.questions.len(), usize::from(!force));
                if force {
                    assert!(!result.unwrap().declined);
                } else {
                    check_decline_or_failure(result, failed);
                    assert_eq!(ui.notices.is_empty(), failed);
                }
            }
        }
    }
}

#[tokio::test]
async fn shortcut_create_remove_and_update_propagate_input_errors_without_losing_prior_reads() {
    let path = ShortcutPath::new("ws", "item", "Files", "Chosen").unwrap();
    let destination = ShortcutCreateDestination {
        workspace: "ws".into(),
        item: "item".into(),
        local_path: "Files/Chosen.Shortcut".into(),
        display_path: "/Team.Workspace/Lake.Lakehouse/Files/Chosen.Shortcut".into(),
        item_type: ItemType::parse("Lakehouse").unwrap(),
    };
    for action in ["create", "remove", "update"] {
        for force in [false, true] {
            for failed in [false, true] {
                let mut ui = Ui::new(if failed {
                    Err(input_error())
                } else {
                    Ok(false)
                });
                let mut operations = Operations::default();
                if action == "update" {
                    operations.reply(
                        ResourceMethod::Get,
                        Some(json!({"name":"Chosen","target":{"type":"OneLake","oneLake":{}}})),
                    );
                }
                if force {
                    operations.reply(
                        if action == "remove" {
                            ResourceMethod::Delete
                        } else {
                            ResourceMethod::Post
                        },
                        Some(json!({"name":"Chosen"})),
                    );
                }
                let mut host = OneLakeShortcutHost {
                    operations: &mut operations,
                    ui: &mut ui,
                };
                match action {
                    "create" => {
                        let result = host
                            .create(
                                &destination,
                                &ShortcutCreateOptions {
                                    input: Some(vec![
                                        json!({
                                            "workspaceId":"other",
                                            "itemId":"target",
                                            "path":"Files",
                                        })
                                        .to_string(),
                                    ]),
                                    force,
                                    ..Default::default()
                                },
                            )
                            .await;
                        if force {
                            assert!(!result.unwrap().declined);
                        } else {
                            check_decline_or_failure(result, failed);
                        }
                    }
                    "remove" => {
                        let result = host.remove(&path, "Chosen.Shortcut", force, true).await;
                        if force {
                            assert!(!result.unwrap().declined);
                        } else {
                            check_decline_or_failure(result, failed);
                        }
                    }
                    _ => {
                        let result = host.update(&path, "name", "Chosen", force).await;
                        if !force && failed {
                            let error = result.unwrap_err();
                            assert_eq!(*error.error, input_error());
                            assert!(error.created.is_none());
                        } else {
                            let result = result.unwrap();
                            assert_eq!(result.declined, !force);
                            assert_eq!(result.created.is_some(), force);
                            assert!(result.deleted.is_none());
                        }
                    }
                }
                assert_eq!(
                    operations.requests.len(),
                    usize::from(action == "update") + usize::from(force)
                );
                assert!(operations.responses.is_empty());
                assert_eq!(ui.questions.len(), usize::from(!force));
            }
        }
    }
}

#[derive(Default)]
struct Catalog {
    item_reads: usize,
    workspace_reads: usize,
}

impl RemovalCatalog for Catalog {
    fn items<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        self.item_reads += 1;
        Box::pin(std::future::ready(Ok(vec![item("Notebook")])))
    }
    fn workspaces<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        self.workspace_reads += 1;
        Box::pin(std::future::ready(Ok(vec![workspace()])))
    }
}

#[tokio::test]
async fn removal_selection_and_confirmation_errors_retain_typed_empty_partial_receipts() {
    for at_tenant in [false, true] {
        for selection_fails in [false, true] {
            let mut ui = Ui::new(Err(input_error()));
            ui.selected = if selection_fails {
                Err(input_error())
            } else {
                Ok(vec![
                    if at_tenant {
                        "Team.Workspace"
                    } else {
                        "Book.Notebook"
                    }
                    .into(),
                ])
            };
            let mut operations = Operations::default();
            let mut catalog = Catalog::default();
            let mut host = RemovalHost {
                operations: &mut operations,
                ui: &mut ui,
                catalog: &mut catalog,
                sort: RemovalSort::ByName,
            };
            let error = if at_tenant {
                host.tenant(&tenant()).await
            } else {
                host.workspace(&workspace(), false, false).await
            }
            .unwrap_err();
            assert_eq!(*error.error, input_error());
            assert!(error.partial.outputs.is_empty());
            assert!(error.partial.responses.is_empty());
            assert!(error.partial.effects.is_empty());
            assert_eq!(error.partial.source_deleted_count, 0);
            assert!(!error.partial.declined);
            assert!(operations.requests.is_empty());
            assert_eq!(catalog.workspace_reads, usize::from(at_tenant));
            assert_eq!(catalog.item_reads, usize::from(!at_tenant));
            assert_eq!(ui.questions.len(), usize::from(!selection_fails));
            assert_eq!(ui.selections.len(), 1);
        }
    }
}

#[tokio::test]
async fn removal_empty_selection_decline_and_force_remain_distinct_from_input_failure() {
    for empty_selection in [false, true] {
        for at_tenant in [false, true] {
            let mut ui = Ui::new(Ok(false));
            if !empty_selection {
                ui.selected = Ok(vec![
                    if at_tenant {
                        "Team.Workspace"
                    } else {
                        "Book.Notebook"
                    }
                    .into(),
                ]);
            }
            let mut operations = Operations::default();
            let mut catalog = Catalog::default();
            let mut host = RemovalHost {
                operations: &mut operations,
                ui: &mut ui,
                catalog: &mut catalog,
                sort: RemovalSort::ByName,
            };
            let result = if at_tenant {
                host.tenant(&tenant()).await
            } else {
                host.workspace(&workspace(), false, false).await
            }
            .unwrap();
            assert!(result.declined);
            assert!(result.responses.is_empty());
            assert!(operations.requests.is_empty());
            assert_eq!(ui.questions.len(), usize::from(!empty_selection));
        }
    }
    let mut ui = Ui::new(Err(input_error()));
    ui.selected = Err(input_error());
    let mut operations = Operations::default();
    operations.reply(ResourceMethod::Delete, None);
    let mut catalog = Catalog::default();
    let result = RemovalHost {
        operations: &mut operations,
        ui: &mut ui,
        catalog: &mut catalog,
        sort: RemovalSort::ByName,
    }
    .workspace(&workspace(), true, false)
    .await
    .unwrap();
    assert!(!result.declined);
    assert_eq!(result.responses.len(), 1);
    assert_eq!(catalog.item_reads, 1);
    assert!(ui.questions.is_empty());
    assert!(ui.selections.is_empty());
}
