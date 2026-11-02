// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Catalog failure, source selection and partial-delete evidence through public APIs.

use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CacheEffect, CommandFuture, MutationError, MutationUi, NoticeKind, RemovalCatalog, RemovalHost,
    RemovalSort, RemovalUi,
};
use fabric_sdk_workspaces::mutations::{
    ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use std::{collections::VecDeque, sync::Arc};

#[derive(Default)]
struct Operations {
    replies: VecDeque<Result<ResourceOutcome, ResourceError>>,
    requests: Vec<String>,
}
impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        self.requests.push(operation.path().into());
        Box::pin(std::future::ready(
            self.replies.pop_front().expect("unexpected removal write"),
        ))
    }
}
#[derive(Default)]
struct Catalog {
    items: Vec<Arc<Element>>,
    workspaces: Vec<Arc<Element>>,
    failed: bool,
}
impl RemovalCatalog for Catalog {
    fn items<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        Box::pin(std::future::ready(if self.failed {
            Err(MutationError::new("Forbidden", "synthetic catalog failure"))
        } else {
            Ok(self.items.clone())
        }))
    }
    fn workspaces<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>> {
        Box::pin(std::future::ready(if self.failed {
            Err(MutationError::new("Forbidden", "synthetic catalog failure"))
        } else {
            Ok(self.workspaces.clone())
        }))
    }
}
#[derive(Default)]
struct Ui {
    selected: Vec<String>,
    confirm: bool,
    prompts: Vec<String>,
}
impl MutationUi for Ui {
    fn notice(&mut self, _: NoticeKind, _: &str) {}
    fn confirm<'a>(&'a mut self, text: &'a str) -> CommandFuture<'a, bool> {
        self.prompts.push(text.into());
        Box::pin(std::future::ready(self.confirm))
    }
}
impl RemovalUi for Ui {
    fn select<'a>(&'a mut self, text: &'a str, _: &'a [String]) -> CommandFuture<'a, Vec<String>> {
        self.prompts.push(text.into());
        Box::pin(std::future::ready(self.selected.clone()))
    }
}
fn workspace() -> Arc<Element> {
    Element::child(
        "Team",
        Some("ws".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap()
}
fn item(name: &str, parent: Arc<Element>) -> Arc<Element> {
    Element::child(
        name,
        Some(name.into()),
        NodeKind::Item(ItemType::parse("Notebook").unwrap()),
        parent,
    )
    .unwrap()
}
fn completed() -> Result<ResourceOutcome, ResourceError> {
    Ok(ResourceOutcome::Completed {
        status: 200,
        data: None,
    })
}

#[tokio::test]
async fn non_force_workspace_removes_only_selected_supported_items_not_the_workspace() {
    let workspace = workspace();
    let mut catalog = Catalog {
        items: vec![
            item("A", Arc::clone(&workspace)),
            item("B", Arc::clone(&workspace)),
        ],
        ..Catalog::default()
    };
    let mut operations = Operations {
        replies: VecDeque::from([completed()]),
        ..Operations::default()
    };
    let mut ui = Ui {
        selected: vec!["B.Notebook".into()],
        confirm: true,
        ..Ui::default()
    };
    let result = RemovalHost {
        operations: &mut operations,
        ui: &mut ui,
        catalog: &mut catalog,
        sort: RemovalSort::ByName,
    }
    .workspace(&workspace, false, true)
    .await
    .unwrap();
    assert_eq!(
        operations.requests,
        ["/v1/workspaces/ws/items/B?hardDelete=true"]
    );
    assert_eq!(result.source_deleted_count, 1);
    assert_eq!(
        result.outputs.last().unwrap().message.as_deref(),
        Some("1 items deleted successfully")
    );
    assert!(
        matches!(&result.effects[..],[CacheEffect::Deleted {target}] if target.name()=="B.Notebook")
    );
}

#[tokio::test]
async fn permission_failed_catalog_never_becomes_empty_workspace_or_permission_to_force_delete() {
    let workspace = workspace();
    let mut catalog = Catalog {
        failed: true,
        ..Catalog::default()
    };
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let error = RemovalHost {
        operations: &mut operations,
        ui: &mut ui,
        catalog: &mut catalog,
        sort: RemovalSort::ByName,
    }
    .workspace(&workspace, true, false)
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "Forbidden");
    assert!(error.partial.responses.is_empty());
    assert!(operations.requests.is_empty());
    assert!(ui.prompts.is_empty());
}

#[tokio::test]
async fn later_failure_keeps_accepted_prior_delete_count_cache_and_output_without_final_summary() {
    let workspace = workspace();
    let mut catalog = Catalog {
        items: vec![
            item("A", Arc::clone(&workspace)),
            item("B", Arc::clone(&workspace)),
        ],
        ..Catalog::default()
    };
    let mut failure = ResourceError::input();
    failure.code = "Forbidden".into();
    failure.status = Some(403);
    failure.mutation_may_have_occurred = true;
    let mut operations = Operations {
        replies: VecDeque::from([
            Ok(ResourceOutcome::Accepted {
                operation_id: Some("pending".into()),
            }),
            Err(failure),
        ]),
        ..Operations::default()
    };
    let mut ui = Ui {
        selected: vec!["A.Notebook".into(), "B.Notebook".into()],
        confirm: true,
        ..Ui::default()
    };
    let error = RemovalHost {
        operations: &mut operations,
        ui: &mut ui,
        catalog: &mut catalog,
        sort: RemovalSort::ByName,
    }
    .workspace(&workspace, false, false)
    .await
    .unwrap_err();
    assert!(error.error.operation.unwrap().mutation_may_have_occurred);
    assert_eq!(error.partial.source_deleted_count, 1);
    assert!(
        matches!(&error.partial.responses[..],[ResourceOutcome::Accepted {operation_id:Some(id)}] if id=="pending")
    );
    assert_eq!(error.partial.outputs.len(), 1);
    assert_eq!(
        error.partial.outputs[0].message.as_deref(),
        Some("'A.Notebook' deleted")
    );
    assert_eq!(operations.requests.len(), 2);
}

#[tokio::test]
async fn cancelled_selection_and_declined_confirmation_do_not_dispatch_a_mutation() {
    let workspace = workspace();
    for selected in [Vec::new(), vec!["A.Notebook".into()]] {
        let mut catalog = Catalog {
            items: vec![item("A", Arc::clone(&workspace))],
            ..Catalog::default()
        };
        let mut operations = Operations::default();
        let mut ui = Ui {
            selected,
            ..Ui::default()
        };
        let result = RemovalHost {
            operations: &mut operations,
            ui: &mut ui,
            catalog: &mut catalog,
            sort: RemovalSort::ByName,
        }
        .workspace(&workspace, false, false)
        .await
        .unwrap();
        assert!(result.declined);
        assert!(result.outputs.is_empty());
        assert!(operations.requests.is_empty());
    }
}

#[tokio::test]
async fn unrelated_catalog_nodes_are_not_silently_selected_for_deletion() {
    let workspace = workspace();
    let other = Element::child(
        "Other",
        Some("other".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap();
    let mut catalog = Catalog {
        items: vec![item("A", other)],
        ..Catalog::default()
    };
    let mut operations = Operations::default();
    let mut ui = Ui::default();
    let error = RemovalHost {
        operations: &mut operations,
        ui: &mut ui,
        catalog: &mut catalog,
        sort: RemovalSort::ByName,
    }
    .workspace(&workspace, true, false)
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "InvalidContext");
    assert!(operations.requests.is_empty());
}
