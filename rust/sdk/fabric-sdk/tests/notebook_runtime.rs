// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native Notebook contracts against synthetic source-derived fixtures.
#![deny(missing_docs, unnameable_test_items)]

use azure_core::http::Url;
use fabric_sdk::{
    notebook_runtime::{
        CommId, CreateSession, ExecutionId, KernelId, MessageId, NotebookRuntime, ResumeBinding,
        SOURCE_REVISION, SessionId,
        content::{
            Cell, CellType, ContentLimits, DefaultLakehouse, NotebookDocument, SaveEffect,
            WritePolicy,
        },
        protocol::{
            CancellationState, CommTarget, ControlReply, ControlRequest, ExecuteRequest,
            ExecuteStatus, ExecutionState, ExecutionTracker, JupyterMessage, LivyStatus,
            Observation, ObservationState, OutputKind, ProtocolLimits, SynapseControl, comm_open,
        },
    },
    runtime::{
        NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod, NativeRequest,
        NativeResponse,
    },
};
use serde_json::{Value, json};
use std::{collections::VecDeque, future::Future, pin::Pin, sync::Mutex};

const BASE: &str = "https://workload.example.invalid/webapi/capacities/cap/workloads/Notebook/Data/Automatic/api/workspaces/ws/artifacts/nb";

fn runtime() -> NotebookRuntime {
    runtime_with_limits(ContentLimits::default(), 1024 * 1024)
}

fn runtime_with_limits(content_limits: ContentLimits, response_bytes: usize) -> NotebookRuntime {
    NotebookRuntime::new(
        NativeEndpoint::new(Url::parse("https://workload.example.invalid").unwrap()).unwrap(),
        "cap",
        "ws",
        "nb",
        content_limits,
        response_bytes,
        100,
    )
    .unwrap()
}

fn document_value() -> Value {
    json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {
            "custom": {"nested": [false, 0, null, "你好"]},
            "dependencies": {"environment": {"id": "env"}}
        },
        "future_notebook": {"keep": [1, null]},
        "cells": [
            {
                "cell_type": "markdown", "id": "md", "metadata": {"tags": ["docs"]},
                "source": ["![image]", "\n", "你好"],
                "attachments": {"pixel.png": {"image/png": "synthetic-base64"}},
                "future_cell": {"keep": true}
            },
            {
                "cell_type": "raw", "metadata": null, "source": "raw\r\ntext",
                "attachments": null, "outputs": null
            },
            {
                "cell_type": "code", "id": "code", "metadata": {"tags": ["keep"]},
                "source": ["print('saved')\n", "1"], "execution_count": 0,
                "outputs": [
                    {"output_type": "stream", "name": "stdout", "text": ["saved\n"], "future": null},
                    {"output_type": "display_data", "data": {"text/plain": ["1"], "application/json": {"n": 1}}, "metadata": null}
                ]
            },
            {"cell_type": "code", "metadata": {}, "source": [], "outputs": null}
        ]
    })
}

fn document() -> NotebookDocument {
    NotebookDocument::from_value(document_value(), ContentLimits::default()).unwrap()
}

fn session() -> SessionId {
    SessionId::new("jupyter-session").unwrap()
}

fn kernel() -> KernelId {
    KernelId::new("kernel").unwrap()
}

fn binding() -> ResumeBinding {
    ResumeBinding {
        session: session(),
        kernel: kernel(),
    }
}

fn execute_request() -> ExecuteRequest {
    ExecuteRequest::new(
        MessageId::new("execute-message").unwrap(),
        session(),
        "print('live')",
        false,
        true,
        ProtocolLimits::default(),
    )
    .unwrap()
}

fn tracker_with_limits(limits: ProtocolLimits) -> ExecutionTracker {
    let mut tracker = ExecutionTracker::new(
        ExecutionId::new("local-operation").unwrap(),
        binding(),
        &execute_request(),
        limits,
    )
    .unwrap();
    tracker.record_submission().unwrap();
    tracker
}

fn execution_tracker() -> ExecutionTracker {
    tracker_with_limits(ProtocolLimits::default())
}

fn inbound(id: &str, parent: &str, kind: &str, content: Value) -> JupyterMessage {
    let channel = if kind == "execute_reply" || kind == "comm_msg" {
        "shell"
    } else {
        "iopub"
    };
    JupyterMessage::from_slice(
        &serde_json::to_vec(&json!({
            "header": {
                "msg_id": id, "msg_type": kind, "session": "kernel-sender-session",
                "username": "kernel", "version": "5.3", "future_header": true
            },
            "parent_header": {"msg_id": parent},
            "channel": channel, "metadata": {"preserved": true}, "content": content,
            "future_envelope": {"preserved": true}
        }))
        .unwrap(),
        ProtocolLimits::default(),
    )
    .unwrap()
}

fn stream(id: &str) -> JupyterMessage {
    inbound(
        id,
        "execute-message",
        "stream",
        json!({"name": "stdout", "text": ["partial\n"]}),
    )
}

fn terminal(status: &str) -> JupyterMessage {
    let mut content = json!({"status": status, "execution_count": 0, "future": null});
    if status == "error" {
        content["ename"] = "ExampleError".into();
        content["evalue"] = "synthetic".into();
        content["traceback"] = json!(["line 1", "line 2"]);
    }
    inbound("terminal", "execute-message", "execute_reply", content)
}

fn cancel_request() -> ControlRequest {
    ControlRequest::new(
        MessageId::new("cancel-control").unwrap(),
        session(),
        CommId::new("control-comm").unwrap(),
        SynapseControl::CancelExecution(MessageId::new("execute-message").unwrap()),
        ProtocolLimits::default(),
    )
    .unwrap()
}

fn cancel_reply(status: &str) -> JupyterMessage {
    inbound(
        "cancel-response",
        "cancel-control",
        "comm_msg",
        json!({
            "comm_id": "control-comm",
            "data": {"parent_msg_id": "cancel-control", "status": status, "response": {}, "error": null}
        }),
    )
}

struct Scripted {
    calls: Mutex<Vec<(NativeRequest, usize)>>,
    replies: Mutex<VecDeque<Result<NativeResponse, NativeError>>>,
}

impl Scripted {
    fn new(replies: Vec<Result<NativeResponse, NativeError>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        }
    }

    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl NativeExecutor for Scripted {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap()
            .push((request.clone(), max_response_bytes));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected dispatch");
        Box::pin(async move { reply })
    }
}

fn response(status: u16, body: Option<Value>) -> Result<NativeResponse, NativeError> {
    Ok(NativeResponse {
        status,
        body: body
            .map(|body| serde_json::to_vec(&body).unwrap())
            .unwrap_or_default(),
        content_type: Some("application/json".into()),
        headers: Default::default(),
    })
}

fn response_with_headers(
    status: u16,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> Result<NativeResponse, NativeError> {
    let mut response = response(status, body)?;
    response.headers = headers
        .iter()
        .map(|(name, value)| ((*name).into(), (*value).into()))
        .collect();
    Ok(response)
}

#[test]
fn source_and_exact_content_routes_and_whole_write_effect() {
    assert_eq!(SOURCE_REVISION, "e250d5d32b04efcc5623a414a361727a93c271ce");
    let runtime = runtime();
    let document = document();
    for (request, method, effect) in [
        (
            runtime.content_get_request().unwrap(),
            NativeMethod::Get,
            NativeEffect::Read,
        ),
        (
            runtime.content_head_request().unwrap(),
            NativeMethod::Head,
            NativeEffect::Read,
        ),
        (
            runtime
                .content_put_request(&document, WritePolicy::AllowUnguarded)
                .unwrap(),
            NativeMethod::Put,
            NativeEffect::Update,
        ),
    ] {
        assert_eq!(request.url().as_str(), format!("{BASE}/content"));
        assert_eq!(request.method(), method);
        assert_eq!(request.effect(), effect);
        assert_eq!(request.accept(), "application/json");
        assert_eq!(request.content_type(), "application/json");
        assert!(request.bytes().is_none());
        assert!(request.headers().is_empty());
        if method == NativeMethod::Put {
            assert_eq!(request.body(), Some(document.as_value()));
        } else {
            assert!(request.body().is_none());
        }
    }
    assert_eq!(document.save_effect(), SaveEffect::WholeNotebook);
    assert!(matches!(
        runtime.content_put_request(&document, WritePolicy::RequireGuarded),
        Err(NativeError::UnsupportedCapability)
    ));
}

#[test]
fn exact_jupyter_collection_get_patch_delete_and_kernel_routes() {
    let runtime = runtime();
    let route = format!("{BASE}/jupyterApi/versions/1/api");
    for (active, suffix) in [(false, "/sessions"), (true, "/sessions/active")] {
        let request = runtime.sessions_request(active).unwrap();
        assert_eq!(request.url().as_str(), format!("{route}{suffix}"));
        assert_eq!(request.method(), NativeMethod::Get);
    }
    let request = runtime.session_get_request(&session(), true).unwrap();
    assert_eq!(
        request.url().as_str(),
        format!("{route}/sessions/jupyter-session?detailed=true")
    );
    assert!(
        runtime
            .session_get_request(&session(), false)
            .unwrap()
            .url()
            .query()
            .is_none()
    );
    let patch = json!({"kernel": {"synapseExtension": {"timeout": 3600}}, "future": null});
    let request = runtime
        .session_patch_request(&session(), patch.clone())
        .unwrap();
    assert_eq!(request.method(), NativeMethod::Patch);
    assert_eq!(
        request.url().as_str(),
        format!("{route}/sessions/jupyter-session")
    );
    assert_eq!(request.body(), Some(&patch));
    assert_eq!(request.effect(), NativeEffect::Update);
    let request = runtime.session_delete_request(&session()).unwrap();
    assert_eq!(request.method(), NativeMethod::Delete);
    assert_eq!(request.effect(), NativeEffect::Delete);
    assert!(request.body().is_none());
    for (request, action, effect) in [
        (
            runtime.kernel_restart_request(&kernel()).unwrap(),
            "restart",
            NativeEffect::Lifecycle,
        ),
        (
            runtime.kernel_interrupt_request(&kernel()).unwrap(),
            "interrupt",
            NativeEffect::Cancel,
        ),
    ] {
        assert_eq!(
            request.url().as_str(),
            format!("{route}/kernels/kernel/{action}")
        );
        assert_eq!(request.method(), NativeMethod::Post);
        assert_eq!(request.effect(), effect);
        assert!(request.body().is_none());
    }
}

#[test]
fn exact_session_create_payload_and_explicit_extension() {
    let options = json!({"language": "python", "defaultLakehouse": {"id": "lh"}});
    let input = CreateSession::new("named", "", "synapse_pyspark", Some(options.clone())).unwrap();
    let expected = json!({
        "name": "named", "path": "/notebook-cli/named", "type": "notebook",
        "kernel": {"name": "synapse_pyspark", "synapseExtension": options}
    });
    assert_eq!(input.as_value(), &expected);
    let request = runtime().session_create_request(&input).unwrap();
    assert_eq!(
        request.url().as_str(),
        format!("{BASE}/jupyterApi/versions/1/api/sessions")
    );
    assert_eq!(request.body(), Some(&expected));
    assert_eq!(request.effect(), NativeEffect::Create);
    assert_eq!(request.method(), NativeMethod::Post);
    let explicit = CreateSession::new("named", "/explicit", "python3", None).unwrap();
    assert_eq!(explicit.as_value()["path"], "/explicit");
    assert!(
        explicit.as_value()["kernel"]
            .get("synapseExtension")
            .is_none()
    );
    let empty_options = CreateSession::new("named", "", "python3", Some(json!({}))).unwrap();
    assert!(
        empty_options.as_value()["kernel"]
            .get("synapseExtension")
            .is_none()
    );
    assert!(CreateSession::new("", "", "python3", None).is_err());
    assert!(CreateSession::new("name", "", "", None).is_err());
    assert!(CreateSession::new("name", "", "python3", Some(json!([]))).is_err());
}

#[test]
fn invalid_identifiers_and_non_origin_endpoints_fail_before_dispatch() {
    for id in ["", ".", "..", "a/b", "a\\b", "a%2fb", "a?b", "a#b", "a\nb"] {
        assert!(SessionId::new(id).is_err());
        assert!(KernelId::new(id).is_err());
        assert!(MessageId::new(id).is_err());
        assert!(CommId::new(id).is_err());
        assert!(ExecutionId::new(id).is_err());
    }
    let endpoint =
        NativeEndpoint::new(Url::parse("https://workload.example.invalid/v1").unwrap()).unwrap();
    assert!(
        NotebookRuntime::new(endpoint, "cap", "ws", "nb", ContentLimits::default(), 1, 1).is_err()
    );
    let runtime = runtime();
    assert!(
        runtime
            .session_get_request(&SessionId::new("active").unwrap(), false)
            .is_err()
    );
    assert!(
        runtime
            .session_patch_request(&session(), json!([]))
            .is_err()
    );
    assert!(
        runtime
            .session_patch_request(&session(), json!({}))
            .is_err()
    );
}

#[test]
fn import_is_lossless_including_unknown_null_absent_source_and_attachments() {
    let expected = document_value();
    let bytes = serde_json::to_vec(&expected).unwrap();
    let document = NotebookDocument::from_slice(&bytes, ContentLimits::default()).unwrap();
    assert_eq!(document.as_value(), &expected);
    assert_eq!(
        document.cell(&document.selector(1).unwrap()).unwrap()["source"],
        "raw\r\ntext"
    );
    assert_eq!(
        document.as_value()["cells"][0]["source"],
        json!(["![image]", "\n", "你好"])
    );
    assert!(
        document.as_value()["cells"][3]
            .get("execution_count")
            .is_none()
    );
    assert_eq!(
        document
            .saved_outputs(&document.selector(3).unwrap())
            .unwrap(),
        Some(&Value::Null)
    );
}

#[test]
fn source_edits_preserve_every_other_field_and_do_not_alias_input() {
    let original = document_value();
    let document =
        NotebookDocument::from_value(original.clone(), ContentLimits::default()).unwrap();
    let selector = document.selector(2).unwrap();
    let updated = document
        .replace_source(&selector, "changed = 2\nlast")
        .unwrap();
    let mut expected = original.clone();
    expected["cells"][2]["source"] = json!(["changed = 2\n", "last"]);
    assert_eq!(updated.as_value(), &expected);
    assert_eq!(document.as_value(), &original);
    assert!(updated.cell(&selector).is_err());
    assert!(document.clone().cell(&selector).is_ok());
    let separately_imported =
        NotebookDocument::from_value(original, ContentLimits::default()).unwrap();
    assert!(separately_imported.cell(&selector).is_err());
}

#[test]
fn cell_creation_insertion_deletion_and_revision_binding() {
    let document = document();
    assert!(document.selector(document.cell_count()).is_err());
    assert!(document.selector(usize::MAX).is_err());
    assert!(document.position(document.cell_count() + 1).is_err());
    let old_position = document.position(0).unwrap();
    let inserted = document
        .insert(&old_position, Cell::new(CellType::Code, ""))
        .unwrap();
    assert_eq!(inserted.cell_count(), 5);
    assert_eq!(
        inserted.as_value()["cells"][0],
        json!({
            "cell_type": "code", "source": [], "metadata": {}, "outputs": [], "execution_count": null
        })
    );
    assert_eq!(
        inserted.as_value()["cells"][1],
        document.as_value()["cells"][0]
    );
    assert!(
        inserted
            .insert(&old_position, Cell::new(CellType::Raw, "x"))
            .is_err()
    );
    let removed = inserted.delete(&inserted.selector(0).unwrap()).unwrap();
    assert_eq!(removed.as_value(), document.as_value());
    let empty = NotebookDocument::from_value(
        json!({"nbformat": 4, "nbformat_minor": 5, "cells": [], "metadata": {}}),
        ContentLimits::default(),
    )
    .unwrap();
    let appended = empty.append(Cell::new(CellType::Markdown, "text")).unwrap();
    assert_eq!(appended.cell_count(), 1);
    assert!(appended.as_value()["cells"][0].get("outputs").is_none());
}

#[test]
fn type_conversion_drops_only_incompatible_fields() {
    let document = document();
    let selector = document.selector(0).unwrap();
    assert_eq!(
        document
            .change_type(&selector, CellType::Markdown)
            .unwrap()
            .as_value(),
        document.as_value()
    );
    let code = document.change_type(&selector, CellType::Code).unwrap();
    assert!(code.as_value()["cells"][0].get("attachments").is_none());
    assert_eq!(code.as_value()["cells"][0]["outputs"], json!([]));
    assert_eq!(code.as_value()["cells"][0]["execution_count"], Value::Null);
    assert_eq!(
        code.as_value()["cells"][0]["future_cell"],
        json!({"keep": true})
    );
    let markdown = document
        .change_type(&document.selector(2).unwrap(), CellType::Markdown)
        .unwrap();
    assert!(markdown.as_value()["cells"][2].get("outputs").is_none());
    assert!(
        markdown.as_value()["cells"][2]
            .get("execution_count")
            .is_none()
    );
    assert_eq!(markdown.as_value()["cells"][2]["id"], "code");
    let raw = document.change_type(&selector, CellType::Raw).unwrap();
    assert_eq!(
        raw.as_value()["cells"][0]["attachments"],
        document.as_value()["cells"][0]["attachments"]
    );
}

#[test]
fn language_and_parameter_edits_are_persisted_metadata_only() {
    let document = document();
    let language = document
        .set_language(&document.selector(2).unwrap(), "sql")
        .unwrap();
    assert_eq!(
        language.as_value()["cells"][2]["metadata"]["language"],
        "sql"
    );
    assert_eq!(
        language.as_value()["cells"][2]["outputs"],
        document.as_value()["cells"][2]["outputs"]
    );
    let enabled = language
        .set_parameter(&language.selector(2).unwrap(), true)
        .unwrap();
    assert_eq!(
        enabled.as_value()["cells"][2]["metadata"]["tags"],
        json!(["keep", "parameters"])
    );
    let again = enabled
        .set_parameter(&enabled.selector(2).unwrap(), true)
        .unwrap();
    assert_eq!(again.as_value(), enabled.as_value());
    let cleared = again
        .set_parameter(&again.selector(2).unwrap(), false)
        .unwrap();
    assert_eq!(
        cleared.as_value()["cells"][2]["metadata"]["tags"],
        json!(["keep"])
    );
}

#[test]
fn content_limits_invalid_types_and_failed_edits_are_non_mutating() {
    let limits = ContentLimits {
        source_bytes: 3,
        ..ContentLimits::default()
    };
    assert!(matches!(
        NotebookDocument::from_value(document_value(), limits),
        Err(NativeError::Limit)
    ));
    let limits = ContentLimits {
        document_bytes: 8,
        ..ContentLimits::default()
    };
    assert!(matches!(
        NotebookDocument::from_slice(b"         ", limits),
        Err(NativeError::Limit)
    ));
    for replacement in [json!("future"), json!(null), json!(4)] {
        let mut value = document_value();
        value["cells"][0]["cell_type"] = replacement;
        assert!(matches!(
            NotebookDocument::from_value(value, ContentLimits::default()),
            Err(NativeError::InvalidInput)
        ));
    }
    for replacement in [json!([1]), json!(null), json!({})] {
        let mut value = document_value();
        value["cells"][0]["source"] = replacement;
        assert!(NotebookDocument::from_value(value, ContentLimits::default()).is_err());
    }
    let document = document();
    assert!(
        document
            .replace_source(&document.selector(2).unwrap(), &"x".repeat(1024 * 1024 + 1))
            .is_err()
    );
    assert_eq!(document.as_value(), &document_value());
    let mut value = document_value();
    value["cells"][0]["metadata"]["tags"] = json!([1]);
    let invalid_tags = NotebookDocument::from_value(value, ContentLimits::default()).unwrap();
    assert!(
        invalid_tags
            .set_parameter(&invalid_tags.selector(0).unwrap(), true)
            .is_err()
    );
}

#[test]
fn cell_count_output_and_total_document_budgets_are_enforced_after_transforms() {
    let limits = ContentLimits {
        cells: 4,
        ..ContentLimits::default()
    };
    let document = NotebookDocument::from_value(document_value(), limits).unwrap();
    assert!(matches!(
        document.append(Cell::new(CellType::Raw, "x")),
        Err(NativeError::Limit)
    ));
    let limits = ContentLimits {
        outputs_per_cell: 1,
        ..ContentLimits::default()
    };
    assert!(matches!(
        NotebookDocument::from_value(document_value(), limits),
        Err(NativeError::Limit)
    ));
    let value = document_value();
    let bytes = serde_json::to_vec(&value).unwrap().len();
    let limits = ContentLimits {
        document_bytes: bytes,
        ..ContentLimits::default()
    };
    let document = NotebookDocument::from_value(value, limits).unwrap();
    assert!(matches!(
        document.replace_source(&document.selector(3).unwrap(), "a much larger source"),
        Err(NativeError::Limit)
    ));
}

#[test]
fn canonical_lakehouse_shadows_legacy_even_when_empty_and_reads_do_not_migrate() {
    let mut value = document_value();
    value["metadata"]["dependencies"] = json!({});
    value["metadata"]["trident"] = json!({"lakehouse": {"default_lakehouse": "legacy"}});
    let document = NotebookDocument::from_value(value.clone(), ContentLimits::default()).unwrap();
    assert!(document.default_lakehouse().unwrap().is_none());
    assert_eq!(document.as_value(), &value);
    value["metadata"]["dependencies"] = Value::Null;
    let document = NotebookDocument::from_value(value.clone(), ContentLimits::default()).unwrap();
    assert_eq!(
        document.default_lakehouse().unwrap().unwrap()["default_lakehouse"],
        "legacy"
    );
    assert_eq!(document.as_value(), &value);
}

#[test]
fn lakehouse_write_preserves_sibling_bindings_and_known_lakehouses() {
    let mut value = document_value();
    value["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("dependencies");
    value["metadata"]["trident"] = json!({
        "environment": {"id": "legacy-env"}, "warehouse": {"id": "warehouse"},
        "lakehouse": {
            "default_lakehouse": "old", "future_binding": [1, null],
            "known_lakehouses": [{"id": "old", "display_name": "Old"}, {"id": "new", "display_name": "Stale", "future": true}]
        }
    });
    let document = NotebookDocument::from_value(value.clone(), ContentLimits::default()).unwrap();
    let edited = document
        .set_default_lakehouse(&DefaultLakehouse {
            id: "new".into(),
            name: "New".into(),
            workspace_id: "other-ws".into(),
        })
        .unwrap();
    let metadata = &edited.as_value()["metadata"];
    assert_eq!(
        metadata["dependencies"]["environment"],
        json!({"id": "legacy-env"})
    );
    assert_eq!(
        metadata["dependencies"]["warehouse"],
        json!({"id": "warehouse"})
    );
    assert_eq!(
        metadata["trident"],
        json!({"environment": {"id": "legacy-env"}, "warehouse": {"id": "warehouse"}})
    );
    let lakehouse = &metadata["dependencies"]["lakehouse"];
    assert_eq!(lakehouse["default_lakehouse_workspace_id"], "other-ws");
    assert_eq!(lakehouse["future_binding"], json!([1, null]));
    assert_eq!(lakehouse["known_lakehouses"].as_array().unwrap().len(), 2);
    assert_eq!(
        lakehouse["known_lakehouses"][1],
        json!({"id": "new", "display_name": "New", "future": true})
    );
    assert_eq!(document.as_value(), &value);
}

#[test]
fn malformed_lakehouse_metadata_never_falls_back_to_legacy() {
    for invalid in [json!([]), json!("bad"), json!(false)] {
        let mut value = document_value();
        value["metadata"]["dependencies"] = invalid;
        value["metadata"]["trident"] = json!({"lakehouse": {"default_lakehouse": "legacy"}});
        let document = NotebookDocument::from_value(value, ContentLimits::default()).unwrap();
        assert!(document.default_lakehouse().is_err());
        assert!(
            document
                .set_default_lakehouse(&DefaultLakehouse {
                    id: "lh".into(),
                    name: "".into(),
                    workspace_id: "ws".into(),
                })
                .is_err()
        );
    }
    let mut value = document_value();
    value["metadata"]["dependencies"]["lakehouse"] = json!({"known_lakehouses": [null]});
    let document = NotebookDocument::from_value(value.clone(), ContentLimits::default()).unwrap();
    assert!(
        document
            .set_default_lakehouse(&DefaultLakehouse {
                id: "lh".into(),
                name: "".into(),
                workspace_id: "ws".into(),
            })
            .is_err()
    );
    assert_eq!(document.as_value(), &value);
}

#[test]
fn canonical_lakehouse_write_keeps_canonical_and_legacy_siblings_separate() {
    let mut value = document_value();
    value["metadata"]["trident"] = json!({
        "lakehouse": {"default_lakehouse": "ignored-legacy"},
        "warehouse": {"id": "legacy-only"}
    });
    let document = NotebookDocument::from_value(value, ContentLimits::default()).unwrap();
    let updated = document
        .set_default_lakehouse(&DefaultLakehouse {
            id: "lh".into(),
            name: "Lakehouse".into(),
            workspace_id: "ws".into(),
        })
        .unwrap();
    assert_eq!(
        updated.as_value()["metadata"]["dependencies"]["environment"],
        json!({"id": "env"})
    );
    assert!(
        updated.as_value()["metadata"]["dependencies"]
            .get("warehouse")
            .is_none()
    );
    assert_eq!(
        updated.as_value()["metadata"]["trident"],
        json!({"warehouse": {"id": "legacy-only"}})
    );
    assert_eq!(
        updated.as_value()["metadata"]["dependencies"]["lakehouse"]["known_lakehouses"],
        json!([{"id": "lh", "display_name": "Lakehouse"}])
    );
}

#[tokio::test]
async fn executor_get_head_and_whole_save_are_exactly_once() {
    let executor = Scripted::new(vec![
        response(200, Some(document_value())),
        response(200, None),
        response(204, None),
    ]);
    let runtime = runtime();
    let document = runtime.get_content(&executor).await.unwrap();
    runtime.head_content(&executor).await.unwrap();
    let edited = document
        .replace_source(&document.selector(2).unwrap(), "changed")
        .unwrap();
    let outcome = runtime
        .put_content(&executor, &edited, WritePolicy::AllowUnguarded)
        .await
        .unwrap();
    assert_eq!(outcome.status, 204);
    assert!(outcome.body.is_none());
    assert_eq!(executor.count(), 3);
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls[0].1, ContentLimits::default().document_bytes);
    assert_eq!(calls[2].0.body(), Some(edited.as_value()));
}

#[tokio::test]
async fn native_session_success_preserves_provider_fields_and_ids() {
    let row = json!({
        "id": "jupyter-session", "name": "name", "path": "/path", "type": "notebook",
        "kernel": {
            "id": "kernel", "name": "synapse_pyspark", "execution_state": "starting",
            "synapse_extension": {"livy_id": "livy-guid", "session_state": "future", "application_id": "app"}
        }, "future": {"preserved": true}
    });
    let executor = Scripted::new(vec![
        response(200, Some(json!([row.clone()]))),
        response(200, Some(json!({"value": [row.clone()]}))),
        response(200, Some(row.clone())),
        response(201, Some(row.clone())),
        response(200, Some(row.clone())),
        response(204, None),
        response(
            200,
            Some(json!({"id": "kernel", "execution_state": "starting"})),
        ),
        response(202, None),
    ]);
    let runtime = runtime();
    let sessions = runtime.list_sessions(&executor, false).await.unwrap();
    assert_eq!(sessions[0].as_value(), &row);
    assert_eq!(sessions[0].id(), &session());
    assert_eq!(sessions[0].kernel_id(), Some(&kernel()));
    assert_eq!(
        runtime.list_sessions(&executor, true).await.unwrap().len(),
        1
    );
    assert_eq!(
        runtime
            .get_session(&executor, &session(), true)
            .await
            .unwrap()
            .as_value(),
        &row
    );
    let create = CreateSession::new("name", "/path", "synapse_pyspark", None).unwrap();
    assert_eq!(
        runtime
            .create_session(&executor, &create)
            .await
            .unwrap()
            .as_value(),
        &row
    );
    assert_eq!(
        runtime
            .patch_session(&executor, &session(), json!({"name": "name"}))
            .await
            .unwrap()
            .as_value(),
        &row
    );
    assert_eq!(
        runtime
            .delete_session(&executor, &session())
            .await
            .unwrap()
            .status,
        204
    );
    assert_eq!(
        runtime
            .restart_kernel(&executor, &kernel())
            .await
            .unwrap()
            .status,
        200
    );
    assert_eq!(
        runtime
            .interrupt_kernel(&executor, &kernel())
            .await
            .unwrap()
            .status,
        202
    );
    assert_eq!(executor.count(), 8);
}

#[tokio::test]
async fn timeout_outcome_unknown_http_and_invalid_response_never_replay() {
    for error in [
        NativeError::TimedOut,
        NativeError::Cancelled,
        NativeError::OutcomeUnknown,
    ] {
        let executor = Scripted::new(vec![Err(error)]);
        assert!(
            matches!(runtime().put_content(&executor, &document(), WritePolicy::AllowUnguarded).await, Err(actual) if actual == error)
        );
        assert_eq!(executor.count(), 1);
    }
    let executor = Scripted::new(vec![response(409, Some(json!({"error": "conflict"})))]);
    assert!(matches!(
        runtime().delete_session(&executor, &session()).await,
        Err(NativeError::Http(409))
    ));
    assert_eq!(executor.count(), 1);
    let executor = Scripted::new(vec![]);
    assert!(matches!(
        runtime()
            .put_content(&executor, &document(), WritePolicy::RequireGuarded)
            .await,
        Err(NativeError::UnsupportedCapability)
    ));
    assert_eq!(executor.count(), 0);
    let executor = Scripted::new(vec![response(200, Some(json!({"not": "an ipynb"})))]);
    assert!(matches!(
        runtime().get_content(&executor).await,
        Err(NativeError::InvalidResponse)
    ));
}

#[tokio::test]
async fn malformed_session_collections_are_not_empty_success_and_ids_are_not_substituted() {
    for value in [
        json!({}),
        json!({"value": null}),
        json!([{}]),
        json!({"value": [], "continuationToken": "more"}),
    ] {
        let executor = Scripted::new(vec![response(200, Some(value))]);
        assert!(runtime().list_sessions(&executor, false).await.is_err());
        assert_eq!(executor.count(), 1);
    }
    let executor = Scripted::new(vec![response(
        200,
        Some(json!({"id": "other", "kernel": {"id": "kernel"}})),
    )]);
    assert!(matches!(
        runtime().get_session(&executor, &session(), false).await,
        Err(NativeError::InvalidResponse)
    ));
}

#[tokio::test]
async fn executor_cannot_bypass_byte_or_session_count_limits() {
    let runtime = NotebookRuntime::new(
        NativeEndpoint::new(Url::parse("https://workload.example.invalid").unwrap()).unwrap(),
        "cap",
        "ws",
        "nb",
        ContentLimits::default(),
        128,
        1,
    )
    .unwrap();
    let executor = Scripted::new(vec![Ok(NativeResponse {
        status: 200,
        body: vec![b' '; 129],
        content_type: Some("application/json".into()),
        headers: Default::default(),
    })]);
    assert!(matches!(
        runtime.list_sessions(&executor, false).await,
        Err(NativeError::Limit)
    ));
    let executor = Scripted::new(vec![response(
        200,
        Some(json!([{"id": "one"}, {"id": "two"}])),
    )]);
    assert!(matches!(
        runtime.list_sessions(&executor, false).await,
        Err(NativeError::Limit)
    ));
}

#[test]
fn websocket_and_resume_require_real_host_even_after_rest_is_available() {
    assert!(matches!(
        runtime().require_websocket_host(),
        Err(NativeError::UnsupportedCapability)
    ));
    assert!(matches!(
        runtime().resume(&binding()),
        Err(NativeError::UnsupportedCapability)
    ));
}

#[test]
fn execute_request_has_exact_source_pinned_payload_and_effect() {
    let request = execute_request();
    let value = serde_json::to_value(request.message()).unwrap();
    assert_eq!(
        value,
        json!({
            "header": {
                "msg_id": "execute-message", "msg_type": "execute_request",
                "username": "notebook-cli", "session": "jupyter-session", "version": "5.3"
            },
            "parent_header": {}, "metadata": {}, "channel": "shell",
            "content": {
                "code": "print('live')", "silent": false, "store_history": true,
                "user_expressions": {}, "allow_stdin": false, "stop_on_error": true
            }
        })
    );
    assert_eq!(request.effect(), NativeEffect::Execute);
    let encoded = request.message().to_vec(ProtocolLimits::default()).unwrap();
    assert!(JupyterMessage::from_slice(&encoded, ProtocolLimits::default()).is_ok());
}

#[test]
fn synapse_control_payload_and_lifecycle_effects_are_distinct() {
    let request = cancel_request();
    assert_eq!(
        request.message().content["data"],
        json!({
            "type": "cancel_cell_execution", "content": "execute-message", "parent_msg_id": "cancel-control"
        })
    );
    assert_eq!(request.effect(), NativeEffect::Cancel);
    for (command, kind, effect) in [
        (
            SynapseControl::CancelAll,
            "cancel_all_cell_execution",
            NativeEffect::Cancel,
        ),
        (
            SynapseControl::StartLivy(json!({})),
            "start_livy_session",
            NativeEffect::Lifecycle,
        ),
        (
            SynapseControl::SetLivyOptions(json!({"language": "python"})),
            "set_livy_session_options",
            NativeEffect::Lifecycle,
        ),
        (
            SynapseControl::ReplayOutputs(MessageId::new("execute-message").unwrap()),
            "replay_outputs",
            NativeEffect::Read,
        ),
        (SynapseControl::Ping, "ping", NativeEffect::Read),
    ] {
        let control = ControlRequest::new(
            MessageId::new("control").unwrap(),
            session(),
            CommId::new("comm").unwrap(),
            command,
            ProtocolLimits::default(),
        )
        .unwrap();
        assert_eq!(control.message().content["data"]["type"], kind);
        assert_eq!(control.effect(), effect);
    }
    let open = comm_open(
        &MessageId::new("open").unwrap(),
        &session(),
        &CommId::new("comm").unwrap(),
        CommTarget::Control,
        ProtocolLimits::default(),
    )
    .unwrap();
    assert_eq!(
        open.content,
        serde_json::from_value::<serde_json::Map<String, Value>>(
            json!({"comm_id": "comm", "target_name": "synapse:control", "data": {}})
        )
        .unwrap()
    );
}

#[test]
fn protocol_preserves_unknown_fields_mime_stream_and_error_traceback() {
    let mut tracker = execution_tracker();
    let stream = stream("stream");
    assert_eq!(tracker.observe(&stream).unwrap(), Observation::Output);
    let mime = inbound(
        "display",
        "execute-message",
        "display_data",
        json!({
            "data": {"text/plain": ["text"], "application/vnd.synapse.table+json": {"rows": [[1, null]]}},
            "metadata": {"custom": true}, "transient": {"display_id": "display"}, "future": [null]
        }),
    );
    let encoded = mime.to_vec(ProtocolLimits::default()).unwrap();
    assert_eq!(
        serde_json::to_value(
            JupyterMessage::from_slice(&encoded, ProtocolLimits::default()).unwrap()
        )
        .unwrap(),
        serde_json::to_value(&mime).unwrap()
    );
    tracker.observe(&mime).unwrap();
    let error = inbound(
        "error",
        "execute-message",
        "error",
        json!({"ename": "ExampleError", "evalue": "synthetic", "traceback": ["one", "two"], "future": 1}),
    );
    tracker.observe(&error).unwrap();
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
    assert_eq!(tracker.outputs()[0].kind, OutputKind::Stream);
    assert_eq!(tracker.outputs()[0].content["text"], json!(["partial\n"]));
    assert_eq!(
        tracker.outputs()[1].mime_data().unwrap()["application/vnd.synapse.table+json"],
        json!({"rows": [[1, null]]})
    );
    assert_eq!(
        tracker.outputs()[2].error().unwrap().unwrap().traceback,
        ["one", "two"]
    );
    tracker.observe(&terminal("error")).unwrap();
    assert_eq!(
        tracker.state(),
        ExecutionState::Finished(ExecuteStatus::Error)
    );
    assert_eq!(
        tracker.reply().unwrap().error.as_ref().unwrap().traceback,
        ["line 1", "line 2"]
    );
    assert_eq!(tracker.reply().unwrap().execution_count, Some(0));
}

#[test]
fn live_execution_never_modifies_saved_cell_outputs() {
    let document = document();
    let selector = document.selector(2).unwrap();
    let saved = document.saved_outputs(&selector).unwrap().cloned();
    let mut tracker = execution_tracker();
    tracker.observe(&stream("stream")).unwrap();
    tracker.observe(&terminal("ok")).unwrap();
    assert_eq!(document.saved_outputs(&selector).unwrap(), saved.as_ref());
    assert_ne!(
        tracker.outputs()[0].content["text"],
        document.as_value()["cells"][2]["outputs"][0]["text"]
    );
}

#[test]
fn prepared_messages_do_not_mean_submitted_and_local_remote_ids_stay_distinct() {
    let request = execute_request();
    let mut tracker = ExecutionTracker::new(
        ExecutionId::new("local-operation").unwrap(),
        binding(),
        &request,
        ProtocolLimits::default(),
    )
    .unwrap();
    assert_eq!(tracker.state(), ExecutionState::Prepared);
    assert_eq!(tracker.observation_state(), ObservationState::NotStarted);
    assert!(tracker.observe(&stream("stream")).is_err());
    assert!(tracker.record_cancellation_sent(&cancel_request()).is_err());
    assert!(
        ExecutionTracker::new(
            ExecutionId::new("execute-message").unwrap(),
            binding(),
            &request,
            ProtocolLimits::default()
        )
        .is_err()
    );
    let wrong = ResumeBinding {
        session: SessionId::new("another").unwrap(),
        kernel: kernel(),
    };
    assert!(
        ExecutionTracker::new(
            ExecutionId::new("local").unwrap(),
            wrong,
            &request,
            ProtocolLimits::default()
        )
        .is_err()
    );
    tracker.record_submission().unwrap();
    assert_ne!(
        tracker.execution_id().as_str(),
        tracker.message_id().as_str()
    );
    assert!(tracker.record_submission().is_err());
}

#[test]
fn unrelated_messages_duplicate_outputs_and_idle_do_not_complete_execution() {
    let mut tracker = execution_tracker();
    let wrong = inbound(
        "wrong",
        "local-operation",
        "execute_reply",
        json!({"status": "ok"}),
    );
    assert_eq!(tracker.observe(&wrong).unwrap(), Observation::Ignored);
    let output = stream("stream");
    assert_eq!(tracker.observe(&output).unwrap(), Observation::Output);
    assert_eq!(tracker.observe(&output).unwrap(), Observation::Ignored);
    assert_eq!(tracker.outputs().len(), 1);
    let idle = inbound(
        "idle",
        "execute-message",
        "status",
        json!({"execution_state": "idle"}),
    );
    assert_eq!(tracker.observe(&idle).unwrap(), Observation::KernelStatus);
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
    assert!(tracker.reply().is_none());
    assert_eq!(
        tracker.observe(&terminal("ok")).unwrap(),
        Observation::Terminal
    );
    assert_eq!(
        tracker.observe(&stream("late")).unwrap(),
        Observation::Ignored
    );
}

#[test]
fn cancel_ack_does_not_finish_execution_and_completion_can_win() {
    for completion in ["ok", "error", "abort"] {
        let mut tracker = execution_tracker();
        tracker.observe(&stream("partial")).unwrap();
        tracker.record_cancellation_sent(&cancel_request()).unwrap();
        assert_eq!(
            tracker.observe(&cancel_reply("ok")).unwrap(),
            Observation::CancellationAck
        );
        assert_eq!(tracker.cancellation_state(), CancellationState::Accepted);
        assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
        tracker.observe(&terminal(completion)).unwrap();
        assert!(matches!(tracker.state(), ExecutionState::Finished(_)));
        assert_eq!(tracker.outputs().len(), 1);
    }
    let mut tracker = execution_tracker();
    tracker.record_cancellation_sent(&cancel_request()).unwrap();
    tracker.observe(&terminal("ok")).unwrap();
    tracker.observe(&cancel_reply("ok")).unwrap();
    assert_eq!(tracker.state(), ExecutionState::Finished(ExecuteStatus::Ok));
    assert_eq!(tracker.cancellation_state(), CancellationState::Accepted);
    assert!(tracker.record_cancellation_sent(&cancel_request()).is_err());
}

#[test]
fn cancellation_rejections_and_mismatches_preserve_partial_outputs_and_remote_state() {
    for status in ["error", "abort", "future"] {
        let mut tracker = execution_tracker();
        tracker.observe(&stream("partial")).unwrap();
        tracker.record_cancellation_sent(&cancel_request()).unwrap();
        tracker.observe(&cancel_reply(status)).unwrap();
        assert_eq!(tracker.cancellation_state(), CancellationState::Rejected);
        assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
        assert_eq!(tracker.outputs().len(), 1);
        tracker.observe(&terminal("ok")).unwrap();
        assert_eq!(tracker.state(), ExecutionState::Finished(ExecuteStatus::Ok));
    }
    for field in ["parent_msg_id", "comm_id"] {
        let mut tracker = execution_tracker();
        tracker.record_cancellation_sent(&cancel_request()).unwrap();
        let mut reply = cancel_reply("ok");
        if field == "parent_msg_id" {
            reply.content["data"]["parent_msg_id"] = "wrong".into();
        } else {
            reply.content["comm_id"] = "wrong".into();
        }
        assert!(matches!(
            tracker.observe(&reply),
            Err(NativeError::InvalidResponse)
        ));
        assert_eq!(tracker.cancellation_state(), CancellationState::AwaitingAck);
    }
}

#[test]
fn cancellation_target_cannot_be_a_different_execution_or_local_handle() {
    let mut tracker = execution_tracker();
    for target in ["other-execute-message", "local-operation"] {
        let request = ControlRequest::new(
            MessageId::new("cancel").unwrap(),
            session(),
            CommId::new("comm").unwrap(),
            SynapseControl::CancelExecution(MessageId::new(target).unwrap()),
            ProtocolLimits::default(),
        )
        .unwrap();
        assert!(tracker.record_cancellation_sent(&request).is_err());
    }
    assert_eq!(
        tracker.cancellation_state(),
        CancellationState::NotRequested
    );
    assert!(
        ControlRequest::new(
            MessageId::new("same").unwrap(),
            session(),
            CommId::new("comm").unwrap(),
            SynapseControl::CancelExecution(MessageId::new("same").unwrap()),
            ProtocolLimits::default(),
        )
        .is_err()
    );
}

#[test]
fn control_reply_acceptance_requires_both_correlations_and_no_error() {
    let request = cancel_request();
    let mut reply = cancel_reply("ok");
    reply.content["data"]["error"] = json!({"ename": "Rejected"});
    let parsed = ControlReply::from_message(&reply, &request, ProtocolLimits::default()).unwrap();
    assert!(!parsed.accepted());
    reply.parent_header.insert("msg_id".into(), "wrong".into());
    assert!(ControlReply::from_message(&reply, &request, ProtocolLimits::default()).is_err());
    let mut source_shape = cancel_reply("ok");
    source_shape.content.remove("comm_id");
    source_shape.channel = "iopub".into();
    assert!(
        ControlReply::from_message(&source_shape, &request, ProtocolLimits::default())
            .unwrap()
            .accepted()
    );
}

#[test]
fn first_cancel_ack_is_not_overwritten_and_sparse_error_reply_is_terminal() {
    let mut tracker = execution_tracker();
    tracker.record_cancellation_sent(&cancel_request()).unwrap();
    tracker.observe(&cancel_reply("ok")).unwrap();
    let mut second = cancel_reply("error");
    second.header.msg_id = "second-cancel-response".into();
    assert_eq!(tracker.observe(&second).unwrap(), Observation::Ignored);
    assert_eq!(tracker.cancellation_state(), CancellationState::Accepted);
    let sparse = inbound(
        "sparse-error",
        "execute-message",
        "execute_reply",
        json!({
            "status": "error", "ename": "Example", "evalue": "synthetic"
        }),
    );
    tracker.observe(&sparse).unwrap();
    assert_eq!(
        tracker.state(),
        ExecutionState::Finished(ExecuteStatus::Error)
    );
    assert!(
        tracker
            .reply()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .traceback
            .is_empty()
    );
}

#[test]
fn timeout_cancel_disconnect_and_resume_never_cancel_replay_or_resubmit() {
    let mut tracker = execution_tracker();
    tracker.observe(&stream("partial")).unwrap();
    tracker.timeout();
    assert!(matches!(
        tracker.observe(&terminal("ok")),
        Err(NativeError::TimedOut)
    ));
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
    assert_eq!(
        tracker.cancellation_state(),
        CancellationState::NotRequested
    );
    assert_eq!(tracker.outputs().len(), 1);
    assert!(tracker.record_submission().is_err());
    let wrong = ResumeBinding {
        session: session(),
        kernel: KernelId::new("other").unwrap(),
    };
    assert!(tracker.resume_observation(&wrong).is_err());
    tracker.resume_observation(&binding()).unwrap();
    tracker.cancel_observation();
    assert!(matches!(
        tracker.observe(&terminal("ok")),
        Err(NativeError::Cancelled)
    ));
    tracker.resume_observation(&binding()).unwrap();
    tracker.disconnect();
    assert_eq!(tracker.observation_state(), ObservationState::Disconnected);
    assert!(tracker.observe(&terminal("ok")).is_err());
    tracker.resume_observation(&binding()).unwrap();
    tracker.observe(&terminal("ok")).unwrap();
    assert_eq!(tracker.message_id().as_str(), "execute-message");
    assert_eq!(tracker.state(), ExecutionState::Finished(ExecuteStatus::Ok));
    assert_eq!(
        tracker.cancellation_state(),
        CancellationState::NotRequested
    );
}

#[test]
fn bounded_protocol_frames_outputs_messages_and_unknown_status_fail_closed() {
    let limits = ProtocolLimits {
        frame_bytes: 8,
        ..ProtocolLimits::default()
    };
    assert!(matches!(
        JupyterMessage::from_slice(b"         ", limits),
        Err(NativeError::Limit)
    ));
    let limits = ProtocolLimits {
        code_bytes: 1,
        ..ProtocolLimits::default()
    };
    assert!(matches!(
        ExecuteRequest::new(
            MessageId::new("id").unwrap(),
            session(),
            "xx",
            false,
            true,
            limits
        ),
        Err(NativeError::Limit)
    ));
    let mut tracker = tracker_with_limits(ProtocolLimits {
        outputs: 1,
        ..ProtocolLimits::default()
    });
    tracker.observe(&stream("one")).unwrap();
    assert!(matches!(
        tracker.observe(&stream("two")),
        Err(NativeError::Limit)
    ));
    assert_eq!(tracker.outputs().len(), 1);
    tracker.observe(&terminal("ok")).unwrap();
    let mut tracker = tracker_with_limits(ProtocolLimits {
        messages: 1,
        ..ProtocolLimits::default()
    });
    tracker.observe(&stream("one")).unwrap();
    assert!(matches!(
        tracker.observe(&terminal("ok")),
        Err(NativeError::Limit)
    ));
    let mut tracker = tracker_with_limits(ProtocolLimits {
        output_bytes: 1,
        ..ProtocolLimits::default()
    });
    assert!(matches!(
        tracker.observe(&stream("one")),
        Err(NativeError::Limit)
    ));
    assert!(tracker.outputs().is_empty());
    let mut tracker = execution_tracker();
    assert!(matches!(
        tracker.observe(&terminal("future")),
        Err(NativeError::InvalidResponse)
    ));
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
}

#[test]
fn malformed_mime_error_and_channel_payloads_do_not_mutate_tracking() {
    let mut tracker = execution_tracker();
    for message in [
        inbound(
            "bad-mime",
            "execute-message",
            "display_data",
            json!({"data": []}),
        ),
        inbound(
            "bad-error",
            "execute-message",
            "error",
            json!({"ename": "Example", "evalue": "x", "traceback": [1]}),
        ),
        inbound(
            "bad-stream",
            "execute-message",
            "stream",
            json!({"name": "stdout", "text": {}}),
        ),
    ] {
        assert!(matches!(
            tracker.observe(&message),
            Err(NativeError::InvalidResponse)
        ));
    }
    let mut wrong_channel = terminal("ok");
    wrong_channel.channel = "iopub".into();
    assert!(tracker.observe(&wrong_channel).is_err());
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
    assert!(tracker.outputs().is_empty());
}

#[test]
fn livy_status_and_kernel_lifecycle_are_not_execution_replies() {
    let message = inbound(
        "livy-status",
        "",
        "comm_msg",
        json!({
            "comm_id": "status", "data": {
                "status_type": "livy_session_status",
                "content": {"session_state": "future-state", "session_error": {"message": "synthetic"}, "application_id": "app", "future": true}
            }
        }),
    );
    let status = LivyStatus::from_message(&message, ProtocolLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(status.session_state.as_deref(), Some("future-state"));
    assert_eq!(status.content["future"], true);
    let mut tracker = execution_tracker();
    assert_eq!(tracker.observe(&message).unwrap(), Observation::Ignored);
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
    assert!(
        LivyStatus::from_message(&terminal("ok"), ProtocolLimits::default())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn small_destination_content_limits_reject_donors_without_changes_or_dispatch() {
    let mut value = document_value();
    value["future_notebook"]["large"] = Value::String("\"\\\n".repeat(64 * 1024));
    let document = NotebookDocument::from_value(value, ContentLimits::default()).unwrap();
    let original = serde_json::to_vec(document.as_value()).unwrap();
    let selector = document.selector(2).unwrap();
    let executor = Scripted::new(vec![]);
    for limits in [
        ContentLimits {
            document_bytes: 64,
            ..ContentLimits::default()
        },
        ContentLimits {
            cells: 1,
            ..ContentLimits::default()
        },
        ContentLimits {
            source_bytes: 1,
            ..ContentLimits::default()
        },
        ContentLimits {
            outputs_per_cell: 1,
            ..ContentLimits::default()
        },
        ContentLimits {
            document_bytes: original.len() - 1,
            ..ContentLimits::default()
        },
    ] {
        let destination = runtime_with_limits(limits, 1024);
        assert!(matches!(
            destination
                .put_content(&executor, &document, WritePolicy::AllowUnguarded)
                .await,
            Err(NativeError::Limit)
        ));
        assert!(document.cell(&selector).is_ok());
        assert_eq!(serde_json::to_vec(document.as_value()).unwrap(), original);
    }
    let destination = runtime_with_limits(
        ContentLimits {
            document_bytes: original.len(),
            ..ContentLimits::default()
        },
        1024,
    );
    let request = destination
        .content_put_request(&document, WritePolicy::AllowUnguarded)
        .unwrap();
    assert_eq!(request.body(), Some(document.as_value()));
    assert_eq!(executor.count(), 0);
}

#[tokio::test]
async fn session_payloads_obey_smaller_destination_budgets_and_preserve_inputs() {
    let input = CreateSession::new(
        "named",
        "/explicit",
        "synapse_pyspark",
        Some(json!({"future": "\"\\\n".repeat(64 * 1024)})),
    )
    .unwrap();
    let original = serde_json::to_vec(input.as_value()).unwrap();
    let executor = Scripted::new(vec![]);
    for bytes in [64, original.len() - 1] {
        let destination = runtime_with_limits(ContentLimits::default(), bytes);
        assert!(matches!(
            destination.create_session(&executor, &input).await,
            Err(NativeError::Limit)
        ));
        assert!(matches!(
            destination
                .patch_session(&executor, &session(), input.as_value().clone())
                .await,
            Err(NativeError::Limit)
        ));
        assert_eq!(serde_json::to_vec(input.as_value()).unwrap(), original);
    }
    let destination = runtime_with_limits(ContentLimits::default(), original.len());
    assert_eq!(
        destination.session_create_request(&input).unwrap().body(),
        Some(input.as_value())
    );
    assert_eq!(executor.count(), 0);
}

#[test]
fn borrowed_protocol_validation_rejects_small_destinations_without_changing_inputs() {
    let mut message = stream("oversized-stream");
    message.content["text"] = Value::String("\"\\\n".repeat(64 * 1024));
    let original = message.to_vec(ProtocolLimits::default()).unwrap();
    let small = ProtocolLimits {
        frame_bytes: 1024,
        ..ProtocolLimits::default()
    };
    assert!(matches!(message.to_vec(small), Err(NativeError::Limit)));
    let mut tracker = tracker_with_limits(small);
    tracker.observe(&stream("partial")).unwrap();
    assert!(matches!(tracker.observe(&message), Err(NativeError::Limit)));
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
    assert_eq!(tracker.outputs().len(), 1);
    assert_eq!(message.to_vec(ProtocolLimits::default()).unwrap(), original);

    let exact = ProtocolLimits {
        frame_bytes: original.len(),
        ..ProtocolLimits::default()
    };
    assert_eq!(message.to_vec(exact).unwrap(), original);
    assert!(matches!(
        message.to_vec(ProtocolLimits {
            frame_bytes: original.len() - 1,
            ..exact
        }),
        Err(NativeError::Limit)
    ));
    let mut reply = cancel_reply("ok");
    reply
        .extra
        .insert("future".into(), message.content["text"].clone());
    let original_reply = reply.to_vec(ProtocolLimits::default()).unwrap();
    assert!(matches!(
        ControlReply::from_message(&reply, &cancel_request(), small),
        Err(NativeError::Limit)
    ));
    assert_eq!(
        reply.to_vec(ProtocolLimits::default()).unwrap(),
        original_reply
    );
}

#[test]
fn outgoing_execute_and_controls_preflight_complete_escaped_wire_sizes() {
    let source = "\"\\\n".repeat(64 * 1024);
    let small = ProtocolLimits {
        frame_bytes: 256,
        ..ProtocolLimits::default()
    };
    assert!(source.len() < small.code_bytes);
    assert!(matches!(
        ExecuteRequest::new(
            MessageId::new("request").unwrap(),
            session(),
            &source,
            false,
            true,
            small
        ),
        Err(NativeError::Limit)
    ));
    let options = json!({"future": source});
    for command in [
        SynapseControl::StartLivy(options.clone()),
        SynapseControl::SetLivyOptions(options.clone()),
    ] {
        assert!(matches!(
            ControlRequest::new(
                MessageId::new("control").unwrap(),
                session(),
                CommId::new("comm").unwrap(),
                command,
                small,
            ),
            Err(NativeError::Limit)
        ));
    }
    assert_eq!(options["future"].as_str(), Some(source.as_str()));
    let request = execute_request();
    let bytes = request
        .message()
        .to_vec(ProtocolLimits::default())
        .unwrap()
        .len();
    for (frame_bytes, accepted) in [(bytes, true), (bytes - 1, false)] {
        let result = ExecuteRequest::new(
            request.id().clone(),
            session(),
            "print('live')",
            false,
            true,
            ProtocolLimits {
                frame_bytes,
                ..ProtocolLimits::default()
            },
        );
        if accepted {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(NativeError::Limit)));
        }
    }
    let control = cancel_request();
    let bytes = control
        .message()
        .to_vec(ProtocolLimits::default())
        .unwrap()
        .len();
    for (frame_bytes, accepted) in [(bytes, true), (bytes - 1, false)] {
        let result = ControlRequest::new(
            control.id().clone(),
            session(),
            CommId::new("control-comm").unwrap(),
            SynapseControl::CancelExecution(MessageId::new("execute-message").unwrap()),
            ProtocolLimits {
                frame_bytes,
                ..ProtocolLimits::default()
            },
        );
        if accepted {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(NativeError::Limit)));
        }
    }
    assert!(matches!(
        comm_open(
            &MessageId::new("open").unwrap(),
            &session(),
            &CommId::new("comm").unwrap(),
            CommTarget::Control,
            ProtocolLimits {
                frame_bytes: 16,
                ..small
            },
        ),
        Err(NativeError::Limit)
    ));
}

#[test]
fn output_destination_limit_precedes_copy_and_preserves_partial_state() {
    let mut tracker = tracker_with_limits(ProtocolLimits {
        output_bytes: 128,
        ..ProtocolLimits::default()
    });
    tracker.observe(&stream("partial")).unwrap();
    let mut message = stream("oversized");
    message.content["text"] = Value::String("\"\\\n".repeat(64 * 1024));
    let original = message.to_vec(ProtocolLimits::default()).unwrap();
    assert!(matches!(tracker.observe(&message), Err(NativeError::Limit)));
    assert_eq!(tracker.outputs().len(), 1);
    assert_eq!(tracker.outputs()[0].content["text"], json!(["partial\n"]));
    assert_eq!(message.to_vec(ProtocolLimits::default()).unwrap(), original);
    assert_eq!(tracker.state(), ExecutionState::AwaitingReply);
    tracker.observe(&stream("oversized")).unwrap();
    assert_eq!(tracker.outputs().len(), 2);
}

#[tokio::test]
async fn head_metadata_observes_case_insensitive_etags_and_absence_verbatim() {
    let runtime = runtime();
    for (name, value) in [("ETag", "\"revision\""), ("eTaG", "W/\"weak-revision\"")] {
        let executor = Scripted::new(vec![response_with_headers(200, None, &[(name, value)])]);
        let metadata = runtime.head_content_metadata(&executor).await.unwrap();
        assert_eq!(metadata.etag(), Some(value));
        assert_eq!(executor.count(), 1);
        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls[0].0.method(), NativeMethod::Head);
        assert_eq!(calls[0].0.url().as_str(), format!("{BASE}/content"));
        assert_eq!(calls[0].0.effect(), NativeEffect::Read);
        assert!(calls[0].0.body().is_none());
        assert!(calls[0].0.headers().is_empty());
    }
    let executor = Scripted::new(vec![response_with_headers(
        200,
        None,
        &[("Content-Type", "application/json")],
    )]);
    assert!(
        runtime
            .head_content_metadata(&executor)
            .await
            .unwrap()
            .etag()
            .is_none()
    );
    assert_eq!(executor.count(), 1);
}

#[tokio::test]
async fn observed_etag_does_not_enable_guarded_writes_or_implicit_if_match() {
    let runtime = runtime();
    let executor = Scripted::new(vec![
        response_with_headers(200, None, &[("ETag", "\"observed-only\"")]),
        response(204, None),
        response_with_headers(200, None, &[("ETag", "\"new-observation\"")]),
    ]);
    assert_eq!(
        runtime.get_etag(&executor).await.unwrap().as_deref(),
        Some("\"observed-only\"")
    );
    let document = document();
    assert!(matches!(
        runtime
            .put_content(&executor, &document, WritePolicy::RequireGuarded)
            .await,
        Err(NativeError::UnsupportedCapability)
    ));
    assert_eq!(executor.count(), 1);
    runtime
        .put_content(&executor, &document, WritePolicy::AllowUnguarded)
        .await
        .unwrap();
    assert_eq!(
        runtime.get_etag(&executor).await.unwrap().as_deref(),
        Some("\"new-observation\"")
    );
    assert_eq!(executor.count(), 3);
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls[1].0.method(), NativeMethod::Put);
    assert_eq!(calls[1].0.body(), Some(document.as_value()));
    assert!(calls[1].0.headers().is_empty());
}

#[tokio::test]
async fn head_metadata_rejects_ambiguous_malformed_and_over_budget_headers() {
    for headers in [
        vec![("ETag", "\"same\""), ("etag", "\"same\"")],
        vec![("ETag", "\"one\""), ("etag", "\"two\"")],
        vec![("ETag", "\"safe\""), ("Other", "bad\r\nheader")],
        vec![("bad header", "value")],
    ] {
        let executor = Scripted::new(vec![response_with_headers(200, None, &headers)]);
        assert!(matches!(
            runtime().head_content_metadata(&executor).await,
            Err(NativeError::InvalidResponse)
        ));
        assert_eq!(executor.count(), 1);
    }
    let long_value = "x".repeat(16 * 1024);
    let executor = Scripted::new(vec![response_with_headers(
        200,
        None,
        &[("ETag", &long_value)],
    )]);
    assert!(matches!(
        runtime().head_content_metadata(&executor).await,
        Err(NativeError::Limit)
    ));
    let mut too_many = response(200, None).unwrap();
    too_many.headers = (0..65)
        .map(|index| (format!("header-{index}"), "value".into()))
        .collect();
    let executor = Scripted::new(vec![Ok(too_many)]);
    assert!(matches!(
        runtime().head_content(&executor).await,
        Err(NativeError::Limit)
    ));
}

#[tokio::test]
async fn head_metadata_preserves_http_failures_and_rejects_non_head_success_shapes() {
    for reply in [
        response(202, None),
        response(204, None),
        response(200, Some(json!({"not": "a HEAD body"}))),
    ] {
        let executor = Scripted::new(vec![reply]);
        assert!(matches!(
            runtime().head_content_metadata(&executor).await,
            Err(NativeError::InvalidResponse)
        ));
        assert_eq!(executor.count(), 1);
    }
    let executor = Scripted::new(vec![response(412, None)]);
    assert!(matches!(
        runtime().get_etag(&executor).await,
        Err(NativeError::Http(412))
    ));
    assert_eq!(executor.count(), 1);
    let executor = Scripted::new(vec![Err(NativeError::TimedOut)]);
    assert!(matches!(
        runtime().get_etag(&executor).await,
        Err(NativeError::TimedOut)
    ));
    assert_eq!(executor.count(), 1);
}

mod managed_driver_tests {
    use super::*;
    use fabric_sdk::notebook_runtime::driver::{
        DeadlineAlarm, DriverEvent, DriverLimits, DriverOptions, DriverState, ManagedExecution,
        NotebookChannel, OutputSink, ReadySession, RemoteOutcome,
    };
    use fabric_sdk::runtime::{NativeDeadline, NativeFuture};
    use std::{sync::Arc, task::{Context, Poll, Waker}, time::Duration};

    #[derive(Default)]
    struct DriverWire {
        inbound: VecDeque<Result<Option<Vec<u8>>, NativeError>>,
        sent: Vec<Vec<u8>>,
        reads: usize,
        caps: Vec<usize>,
        writes: Vec<(usize, fabric_sdk::notebook_runtime::protocol::LiveOutput)>,
        closes: usize,
        drops: usize,
        deadlines: Vec<usize>,
        pending_send: bool,
        pending_read: bool,
        pending_close: bool,
        send_error: Option<NativeError>,
        close_error: Option<NativeError>,
        drift: Option<ReadySession>,
    }

    struct DriverChannel {
        ready: ReadySession,
        wire: Arc<Mutex<DriverWire>>,
    }

    impl Drop for DriverChannel {
        fn drop(&mut self) {
            self.wire.lock().unwrap().drops += 1;
        }
    }

    impl NotebookChannel for DriverChannel {
        fn binding(&self) -> &ReadySession {
            &self.ready
        }

        fn send<'a>(
            &'a mut self,
            frame: &'a [u8],
            deadline: &'a NativeDeadline,
        ) -> NativeFuture<'a, Result<(), NativeError>> {
            Box::pin(async move {
                let (pending, error, drift) = {
                    let mut wire = self.wire.lock().unwrap();
                    wire.sent.push(frame.to_vec());
                    wire.deadlines.push(deadline as *const NativeDeadline as usize);
                    (wire.pending_send, wire.send_error.take(), wire.drift.take())
                };
                if let Some(ready) = drift {
                    self.ready = ready;
                }
                if pending {
                    std::future::pending::<()>().await;
                }
                error.map_or(Ok(()), Err)
            })
        }

        fn receive<'a>(
            &'a mut self,
            max_bytes: usize,
            deadline: &'a NativeDeadline,
        ) -> NativeFuture<'a, Result<Option<Vec<u8>>, NativeError>> {
            Box::pin(async move {
                let (pending, reply) = {
                    let mut wire = self.wire.lock().unwrap();
                    wire.reads += 1;
                    wire.caps.push(max_bytes);
                    wire.deadlines.push(deadline as *const NativeDeadline as usize);
                    (wire.pending_read, wire.inbound.pop_front().unwrap_or(Ok(None)))
                };
                if pending {
                    std::future::pending::<()>().await;
                }
                reply
            })
        }

        fn close<'a>(
            &'a mut self,
            deadline: &'a NativeDeadline,
        ) -> NativeFuture<'a, Result<(), NativeError>> {
            Box::pin(async move {
                let (pending, error) = {
                    let mut wire = self.wire.lock().unwrap();
                    wire.closes += 1;
                    wire.deadlines.push(deadline as *const NativeDeadline as usize);
                    (wire.pending_close, wire.close_error.take())
                };
                if pending {
                    std::future::pending::<()>().await;
                }
                error.map_or(Ok(()), Err)
            })
        }
    }

    struct DriverAlarm(Arc<Mutex<DriverWire>>);

    impl DeadlineAlarm for DriverAlarm {
        fn wait<'a>(&'a self, deadline: &'a NativeDeadline) -> NativeFuture<'a, ()> {
            Box::pin(async move {
                self.0.lock().unwrap().deadlines.push(deadline as *const NativeDeadline as usize);
                if let Ok(remaining) = deadline.remaining() {
                    tokio::time::sleep(remaining).await;
                }
            })
        }
    }

    struct DriverSink {
        wire: Arc<Mutex<DriverWire>>,
        gate: Option<Arc<tokio::sync::Notify>>,
        error: Option<NativeError>,
    }

    impl OutputSink for DriverSink {
        fn write<'a>(
            &'a mut self,
            index: usize,
            output: &'a fabric_sdk::notebook_runtime::protocol::LiveOutput,
            max_bytes: usize,
            deadline: &'a NativeDeadline,
        ) -> NativeFuture<'a, Result<(), NativeError>> {
            Box::pin(async move {
                assert_eq!(serde_json::to_vec(&output.content).unwrap().len(), max_bytes);
                {
                    let mut wire = self.wire.lock().unwrap();
                    wire.writes.push((index, output.clone()));
                    wire.deadlines.push(deadline as *const NativeDeadline as usize);
                }
                if let Some(gate) = &self.gate {
                    gate.notified().await;
                }
                self.error.map_or(Ok(()), Err)
            })
        }
    }

    fn ready_driver_session() -> ReadySession {
        ReadySession::new(&runtime(), binding(), CommId::new("control-comm").unwrap())
    }

    fn driver_wire(frames: Vec<JupyterMessage>) -> Arc<Mutex<DriverWire>> {
        Arc::new(Mutex::new(DriverWire {
            inbound: frames.into_iter()
                .map(|frame| Ok(Some(frame.to_vec(ProtocolLimits::default()).unwrap())))
                .collect(),
            ..DriverWire::default()
        }))
    }

    fn driver_options(limits: DriverLimits, deadline: NativeDeadline) -> DriverOptions {
        DriverOptions {
            ready: ready_driver_session(),
            execution: ExecutionId::new("local-operation").unwrap(),
            limits,
            deadline,
        }
    }

    fn managed_driver(
        wire: &Arc<Mutex<DriverWire>>,
        limits: DriverLimits,
        deadline: NativeDeadline,
    ) -> ManagedExecution {
        ManagedExecution::new(
            Box::new(DriverChannel { ready: ready_driver_session(), wire: Arc::clone(wire) }),
            Box::new(DriverAlarm(Arc::clone(wire))),
            &execute_request(),
            driver_options(limits, deadline),
        ).unwrap()
    }

    fn driver_sink(wire: &Arc<Mutex<DriverWire>>) -> DriverSink {
        DriverSink { wire: Arc::clone(wire), gate: None, error: None }
    }

    fn driver_deadline() -> NativeDeadline {
        NativeDeadline::new(Duration::from_secs(30)).unwrap()
    }

    fn assert_driver_pending<F: Future>(mut future: Pin<&mut F>) {
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    }

    #[tokio::test]
    async fn driver_sends_actual_code_once_and_observes_native_workload_outcomes() {
        for (status, expected) in [
            ("ok", ExecuteStatus::Ok), ("error", ExecuteStatus::Error), ("abort", ExecuteStatus::Abort),
        ] {
            let unrelated = inbound("other", "other-execution", "stream", json!({"name":"stdout","text":"not ours"}));
            let idle = inbound("idle", "execute-message", "status", json!({"execution_state":"idle"}));
            let wire = driver_wire(vec![unrelated, idle, stream("out"), terminal(status)]);
            let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
            let mut sink = driver_sink(&wire);
            assert_eq!(driver.remote_outcome(), RemoteOutcome::NotSubmitted);
            assert_eq!(driver.run(&mut sink).await.unwrap(), expected);
            assert_eq!(driver.remote_outcome(), RemoteOutcome::Finished(expected));
            assert_eq!(driver.state(), DriverState::Finished);
            assert_eq!(driver.delivered_outputs(), 1);
            assert_eq!(driver.received_frames(), 4);
            assert_eq!(driver.observer().outputs().len(), 1);
            assert_eq!(driver.run(&mut sink).await.unwrap(), expected);
            assert!(driver.submit().await.is_err());
            let wire = wire.lock().unwrap();
            assert_eq!(wire.sent.len(), 1);
            let sent = JupyterMessage::from_slice(&wire.sent[0], ProtocolLimits::default()).unwrap();
            assert_eq!(serde_json::to_value(sent).unwrap(), serde_json::to_value(execute_request().message()).unwrap());
            assert_eq!(wire.writes[0].0, 0);
            assert_eq!(wire.writes[0].1.content["text"], json!(["partial\n"]));
            assert_eq!(wire.reads, 4);
            assert_eq!(wire.closes, 0);
        }
    }

    #[tokio::test]
    async fn driver_streams_mime_and_tracebacks_without_confusing_error_output_with_terminal() {
        let rich = inbound("rich", "execute-message", "display_data", json!({
            "data":{"text/plain":["one"],"application/json":{"nested":[1,null]}},
            "metadata":{"future":true}
        }));
        let error = inbound("error", "execute-message", "error", json!({
            "ename":"Example","evalue":"synthetic","traceback":["first","second"]
        }));
        let wire = driver_wire(vec![rich, error, terminal("error")]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        let mut sink = driver_sink(&wire);
        driver.submit().await.unwrap();
        assert_eq!(driver.observe_next(&mut sink).await.unwrap(), DriverEvent::Output(0));
        assert_eq!(driver.observe_next(&mut sink).await.unwrap(), DriverEvent::Output(1));
        assert_eq!(driver.remote_outcome(), RemoteOutcome::Unknown);
        assert_eq!(driver.run(&mut sink).await.unwrap(), ExecuteStatus::Error);
        assert_eq!(driver.observer().outputs()[0].mime_data().unwrap()["application/json"], json!({"nested":[1,null]}));
        assert_eq!(driver.observer().outputs()[1].error().unwrap().unwrap().traceback, ["first", "second"]);
        assert!(driver.observer().reply().unwrap().error.is_some());
    }

    #[tokio::test]
    async fn driver_applies_sink_backpressure_before_pulling_another_frame() {
        let wire = driver_wire(vec![stream("out"), terminal("ok")]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        driver.submit().await.unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        let mut sink = driver_sink(&wire);
        sink.gate = Some(Arc::clone(&gate));
        let mut pending = Box::pin(driver.observe_next(&mut sink));
        assert_driver_pending(pending.as_mut());
        assert_eq!(wire.lock().unwrap().reads, 1);
        assert_eq!(wire.lock().unwrap().writes.len(), 1);
        gate.notify_one();
        assert_eq!(pending.await.unwrap(), DriverEvent::Output(0));
        assert_eq!(driver.delivered_outputs(), 1);
        assert_eq!(driver.run(&mut sink).await.unwrap(), ExecuteStatus::Ok);
        assert_eq!(wire.lock().unwrap().reads, 2);
    }

    #[tokio::test]
    async fn driver_sink_failure_preserves_partial_output_and_poisons_continuation() {
        let wire = driver_wire(vec![stream("out"), terminal("ok")]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        let mut sink = driver_sink(&wire);
        sink.error = Some(NativeError::Limit);
        let failure = driver.run(&mut sink).await.unwrap_err();
        assert_eq!(failure.cause, NativeError::Limit);
        assert_eq!(failure.remote, RemoteOutcome::Unknown);
        assert_eq!(driver.observer().outputs().len(), 1);
        assert_eq!(driver.delivered_outputs(), 0);
        assert_eq!(driver.state(), DriverState::Failed);
        assert!(driver.run(&mut sink).await.is_err());
        assert!(driver.submit().await.is_err());
        assert!(driver.cancel(MessageId::new("cancel").unwrap()).await.is_err());
        assert_eq!(wire.lock().unwrap().reads, 1);
        assert_eq!(wire.lock().unwrap().sent.len(), 1);
    }

    #[tokio::test]
    async fn driver_bounds_unrelated_and_duplicate_traffic_independently_of_observer() {
        for frame in [
            inbound("unrelated", "other", "execute_reply", json!({"status":"ok"})),
            stream("duplicate"),
        ] {
            let wire = driver_wire(vec![frame.clone(), frame.clone(), terminal("ok")]);
            let limits = DriverLimits { frames: 2, ..DriverLimits::default() };
            let mut driver = managed_driver(&wire, limits, driver_deadline());
            let mut sink = driver_sink(&wire);
            let failure = driver.run(&mut sink).await.unwrap_err();
            assert_eq!(failure.cause, NativeError::Limit);
            assert_eq!(failure.remote, RemoteOutcome::Unknown);
            assert_eq!(wire.lock().unwrap().reads, 2);
            assert_eq!(driver.received_frames(), 2);
        }
    }

    #[tokio::test]
    async fn driver_rejects_malformed_oversized_and_owner_session_frames() {
        let mut wrong_session = terminal("ok");
        wrong_session.parent_header.insert("session".into(), "different-session".into());
        for (bad, expected) in [
            (b"not-json".to_vec(), NativeError::InvalidResponse),
            (vec![b' '; 1025], NativeError::Limit),
            (wrong_session.to_vec(ProtocolLimits::default()).unwrap(), NativeError::InvalidResponse),
        ] {
            let wire = driver_wire(vec![stream("partial")]);
            wire.lock().unwrap().inbound.push_back(Ok(Some(bad)));
            let limits = DriverLimits {
                protocol: ProtocolLimits { frame_bytes: 1024, ..ProtocolLimits::default() },
                ..DriverLimits::default()
            };
            let mut driver = managed_driver(&wire, limits, driver_deadline());
            let failure = driver.run(&mut driver_sink(&wire)).await.unwrap_err();
            assert_eq!(failure.cause, expected);
            assert_eq!(failure.remote, RemoteOutcome::Unknown);
            assert_eq!(driver.delivered_outputs(), 1);
            assert_eq!(driver.observer().outputs().len(), 1);
            assert!(driver.submit().await.is_err());
        }
    }

    #[tokio::test]
    async fn driver_total_wire_byte_budget_limits_each_pull_and_does_not_read_ahead() {
        let frame = stream("out");
        let bytes = frame.to_vec(ProtocolLimits::default()).unwrap().len();
        let wire = driver_wire(vec![frame, terminal("ok")]);
        let limits = DriverLimits { received_bytes: bytes, ..DriverLimits::default() };
        let mut driver = managed_driver(&wire, limits, driver_deadline());
        let failure = driver.run(&mut driver_sink(&wire)).await.unwrap_err();
        assert_eq!(failure.cause, NativeError::Limit);
        assert_eq!(driver.received_bytes(), bytes);
        assert_eq!(wire.lock().unwrap().caps, vec![bytes]);
        assert_eq!(wire.lock().unwrap().reads, 1);
    }

    #[test]
    fn driver_preflights_destination_limits_and_ids_before_any_send() {
        for limits in [
            DriverLimits { frames: 0, ..DriverLimits::default() },
            DriverLimits { received_bytes: 0, ..DriverLimits::default() },
            DriverLimits {
                protocol: ProtocolLimits { frame_bytes: 1, ..ProtocolLimits::default() },
                ..DriverLimits::default()
            },
            DriverLimits {
                protocol: ProtocolLimits { outputs: 0, ..ProtocolLimits::default() },
                ..DriverLimits::default()
            },
            DriverLimits {
                protocol: ProtocolLimits { code_bytes: 1, ..ProtocolLimits::default() },
                ..DriverLimits::default()
            },
        ] {
            let wire = driver_wire(vec![]);
            let result = ManagedExecution::new(
                Box::new(DriverChannel { ready: ready_driver_session(), wire: Arc::clone(&wire) }),
                Box::new(DriverAlarm(Arc::clone(&wire))),
                &execute_request(),
                driver_options(limits, driver_deadline()),
            );
            assert!(result.is_err());
            assert!(wire.lock().unwrap().sent.is_empty());
            assert_eq!(wire.lock().unwrap().drops, 1);
        }
        let wire = driver_wire(vec![]);
        let other = ExecuteRequest::new(
            MessageId::new("request").unwrap(), SessionId::new("other-session").unwrap(),
            "1", false, true, ProtocolLimits::default(),
        ).unwrap();
        let result = ManagedExecution::new(
            Box::new(DriverChannel { ready: ready_driver_session(), wire: Arc::clone(&wire) }),
            Box::new(DriverAlarm(Arc::clone(&wire))), &other,
            driver_options(DriverLimits::default(), driver_deadline()),
        );
        assert!(matches!(result, Err(NativeError::InvalidInput)));
        assert!(wire.lock().unwrap().sent.is_empty());
    }

    #[test]
    fn driver_rejects_owner_kernel_and_comm_mismatch_without_channel_io() {
        let other_notebook = NotebookRuntime::new(
            NativeEndpoint::new(Url::parse("https://other.example.invalid").unwrap()).unwrap(),
            "cap", "other-workspace", "other-notebook", ContentLimits::default(), 1024, 10,
        ).unwrap();
        let other_kernel = ResumeBinding { session: session(), kernel: KernelId::new("other").unwrap() };
        for ready in [
            ReadySession::new(&other_notebook, binding(), CommId::new("control-comm").unwrap()),
            ReadySession::new(&runtime(), other_kernel, CommId::new("control-comm").unwrap()),
            ReadySession::new(&runtime(), binding(), CommId::new("other-comm").unwrap()),
        ] {
            let wire = driver_wire(vec![]);
            let result = ManagedExecution::new(
                Box::new(DriverChannel { ready, wire: Arc::clone(&wire) }),
                Box::new(DriverAlarm(Arc::clone(&wire))), &execute_request(),
                driver_options(DriverLimits::default(), driver_deadline()),
            );
            assert!(matches!(result, Err(NativeError::InvalidInput)));
            let wire = wire.lock().unwrap();
            assert!(wire.sent.is_empty());
            assert_eq!(wire.reads, 0);
            assert_eq!(wire.drops, 1);
        }
    }

    #[tokio::test]
    async fn driver_lost_after_send_and_send_failure_remain_unknown_and_never_resubmit() {
        for send_error in [None, Some(NativeError::OutcomeUnknown)] {
            let wire = driver_wire(vec![]);
            wire.lock().unwrap().send_error = send_error;
            let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
            let failure = driver.run(&mut driver_sink(&wire)).await.unwrap_err();
            assert_eq!(failure.cause, NativeError::OutcomeUnknown);
            assert_eq!(failure.remote, RemoteOutcome::Unknown);
            assert!(driver.submit().await.is_err());
            assert!(driver.run(&mut driver_sink(&wire)).await.is_err());
            assert_eq!(wire.lock().unwrap().sent.len(), 1);
        }
        let wire = driver_wire(vec![stream("partial")]);
        wire.lock().unwrap().inbound.push_back(Err(NativeError::InvalidResponse));
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        let failure = driver.run(&mut driver_sink(&wire)).await.unwrap_err();
        assert_eq!(failure.remote, RemoteOutcome::Unknown);
        assert_eq!(driver.delivered_outputs(), 1);
    }

    #[tokio::test]
    async fn driver_dropped_send_is_unknown_and_cannot_be_submitted_again() {
        let wire = driver_wire(vec![]);
        wire.lock().unwrap().pending_send = true;
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        let mut send = Box::pin(driver.submit());
        assert_driver_pending(send.as_mut());
        drop(send);
        assert_eq!(driver.state(), DriverState::SubmissionUnknown);
        assert_eq!(driver.remote_outcome(), RemoteOutcome::Unknown);
        assert_eq!(driver.submit().await.unwrap_err().cause, NativeError::OutcomeUnknown);
        assert_eq!(wire.lock().unwrap().sent.len(), 1);
    }

    #[tokio::test]
    async fn driver_dropped_receive_or_sink_await_poisons_continuation() {
        for block_receive in [true, false] {
            let wire = driver_wire(vec![stream("out"), terminal("ok")]);
            wire.lock().unwrap().pending_read = block_receive;
            let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
            driver.submit().await.unwrap();
            let mut sink = driver_sink(&wire);
            if !block_receive {
                sink.gate = Some(Arc::new(tokio::sync::Notify::new()));
            }
            let mut observation = Box::pin(driver.observe_next(&mut sink));
            assert_driver_pending(observation.as_mut());
            drop(observation);
            assert_eq!(driver.state(), DriverState::Interrupted);
            assert_eq!(driver.remote_outcome(), RemoteOutcome::Unknown);
            assert_eq!(driver.observe_next(&mut sink).await.unwrap_err().cause, NativeError::Cancelled);
            assert_eq!(wire.lock().unwrap().reads, 1);
            assert_eq!(wire.lock().unwrap().sent.len(), 1);
        }
    }

    #[tokio::test]
    async fn driver_alarm_bounds_stalled_send_receive_and_sink_without_io_wakeups() {
        for stage in ["send", "receive", "sink"] {
            let wire = driver_wire(vec![stream("out"), terminal("ok")]);
            {
                let mut wire = wire.lock().unwrap();
                wire.pending_send = stage == "send";
                wire.pending_read = stage == "receive";
            }
            let deadline = NativeDeadline::new(Duration::from_millis(100)).unwrap();
            let mut driver = managed_driver(&wire, DriverLimits::default(), deadline);
            let mut sink = driver_sink(&wire);
            if stage == "sink" {
                sink.gate = Some(Arc::new(tokio::sync::Notify::new()));
            }
            let failure = tokio::time::timeout(Duration::from_secs(2), driver.run(&mut sink))
                .await.expect("driver did not enforce its pending-I/O deadline").unwrap_err();
            assert_eq!(failure.cause, NativeError::TimedOut);
            assert_eq!(failure.remote, RemoteOutcome::Unknown);
            assert_eq!(wire.lock().unwrap().sent.len(), 1);
            assert_eq!(driver.state(), DriverState::Failed);
            assert!(driver.submit().await.is_err());
        }
    }

    #[tokio::test]
    async fn driver_uses_one_absolute_deadline_and_cancel_ack_is_not_terminal() {
        let wire = driver_wire(vec![cancel_reply("ok"), stream("partial"), terminal("error")]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        let mut sink = driver_sink(&wire);
        driver.submit().await.unwrap();
        driver.cancel(MessageId::new("cancel-control").unwrap()).await.unwrap();
        assert_eq!(driver.observe_next(&mut sink).await.unwrap(), DriverEvent::CancellationAck(true));
        assert_eq!(driver.remote_outcome(), RemoteOutcome::Unknown);
        assert!(driver.cancel(MessageId::new("second-control").unwrap()).await.is_err());
        assert_eq!(driver.run(&mut sink).await.unwrap(), ExecuteStatus::Error);
        driver.close().await.unwrap();
        let wire = wire.lock().unwrap();
        assert_eq!(wire.sent.len(), 2);
        let cancel = JupyterMessage::from_slice(&wire.sent[1], ProtocolLimits::default()).unwrap();
        assert_eq!(cancel.content["comm_id"], "control-comm");
        assert_eq!(cancel.content["data"]["content"], "execute-message");
        assert_eq!(cancel.content["data"]["type"], "cancel_cell_execution");
        assert!(wire.deadlines.len() > 5);
        assert!(wire.deadlines.iter().all(|address| *address == wire.deadlines[0]));
    }

    #[tokio::test]
    async fn driver_completion_can_win_cancel_race_without_fabricating_an_ack() {
        let wire = driver_wire(vec![terminal("ok"), cancel_reply("ok")]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        driver.submit().await.unwrap();
        driver.cancel(MessageId::new("cancel-control").unwrap()).await.unwrap();
        assert_eq!(driver.run(&mut driver_sink(&wire)).await.unwrap(), ExecuteStatus::Ok);
        assert_eq!(driver.observer().cancellation_state(), CancellationState::AwaitingAck);
        assert_eq!(wire.lock().unwrap().reads, 1);
    }

    #[tokio::test]
    async fn driver_close_and_drop_dispose_local_channel_without_remote_cleanup() {
        let wire = driver_wire(vec![]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        driver.close().await.unwrap();
        driver.close().await.unwrap();
        assert_eq!(driver.remote_outcome(), RemoteOutcome::NotSubmitted);
        assert_eq!(driver.state(), DriverState::Closed);
        assert_eq!(wire.lock().unwrap().closes, 1);
        assert_eq!(wire.lock().unwrap().drops, 1);
        assert!(wire.lock().unwrap().sent.is_empty());
        drop(driver);
        assert_eq!(wire.lock().unwrap().drops, 1);
        let wire = driver_wire(vec![]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        driver.submit().await.unwrap();
        drop(driver);
        assert_eq!(wire.lock().unwrap().drops, 1);
        assert_eq!(wire.lock().unwrap().closes, 0);
        assert_eq!(wire.lock().unwrap().sent.len(), 1);
    }

    #[tokio::test]
    async fn driver_close_error_timeout_and_drop_still_dispose_channel_once() {
        for stage in ["error", "timeout", "drop"] {
            let wire = driver_wire(vec![]);
            {
                let mut wire = wire.lock().unwrap();
                wire.close_error = (stage == "error").then_some(NativeError::OutcomeUnknown);
                wire.pending_close = stage != "error";
            }
            let deadline = NativeDeadline::new(Duration::from_millis(100)).unwrap();
            let mut driver = managed_driver(&wire, DriverLimits::default(), deadline);
            if stage == "drop" {
                let mut close = Box::pin(driver.close());
                assert_driver_pending(close.as_mut());
                drop(close);
            } else {
                let failure = tokio::time::timeout(Duration::from_secs(2), driver.close()).await.unwrap().unwrap_err();
                assert_eq!(failure.remote, RemoteOutcome::NotSubmitted);
                assert_eq!(failure.cause, if stage == "timeout" { NativeError::TimedOut } else { NativeError::OutcomeUnknown });
            }
            assert_eq!(driver.state(), DriverState::Closed);
            assert_eq!(wire.lock().unwrap().drops, 1);
            driver.close().await.unwrap();
            assert_eq!(wire.lock().unwrap().closes, 1);
            assert!(wire.lock().unwrap().sent.is_empty());
        }
    }

    #[tokio::test]
    async fn driver_expired_original_deadline_prevents_submit_and_close_io() {
        let wire = driver_wire(vec![]);
        let deadline = NativeDeadline::new(Duration::from_millis(100)).unwrap();
        let mut driver = managed_driver(&wire, DriverLimits::default(), deadline);
        tokio::time::sleep(Duration::from_millis(120)).await;
        let failure = driver.submit().await.unwrap_err();
        assert_eq!(failure.cause, NativeError::TimedOut);
        assert_eq!(failure.remote, RemoteOutcome::NotSubmitted);
        assert!(wire.lock().unwrap().sent.is_empty());
        assert_eq!(driver.close().await.unwrap_err().cause, NativeError::TimedOut);
        assert_eq!(wire.lock().unwrap().closes, 0);
        assert_eq!(wire.lock().unwrap().drops, 1);
    }

    #[tokio::test]
    async fn driver_owner_drift_after_send_is_unknown_and_stops_observation() {
        let wire = driver_wire(vec![terminal("ok")]);
        wire.lock().unwrap().drift = Some(ReadySession::new(
            &runtime(), binding(), CommId::new("changed-comm").unwrap(),
        ));
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        let failure = driver.submit().await.unwrap_err();
        assert_eq!(failure.cause, NativeError::InvalidInput);
        assert_eq!(failure.remote, RemoteOutcome::Unknown);
        assert_eq!(wire.lock().unwrap().reads, 0);
        assert!(driver.run(&mut driver_sink(&wire)).await.is_err());
        let debug = format!("{driver:?} {failure:?} {}", failure);
        for private in ["execute-message", "jupyter-session", "print('live')", "changed-comm", "example.invalid"] {
            assert!(!debug.contains(private));
        }
    }

    #[tokio::test]
    async fn driver_cancel_rejection_does_not_finish_or_drop_partial_outputs() {
        let wire = driver_wire(vec![stream("partial"), cancel_reply("error"), terminal("ok")]);
        let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
        let mut sink = driver_sink(&wire);
        driver.submit().await.unwrap();
        driver.cancel(MessageId::new("cancel-control").unwrap()).await.unwrap();
        assert_eq!(driver.observe_next(&mut sink).await.unwrap(), DriverEvent::Output(0));
        assert_eq!(driver.observe_next(&mut sink).await.unwrap(), DriverEvent::CancellationAck(false));
        assert_eq!(driver.observer().cancellation_state(), CancellationState::Rejected);
        assert_eq!(driver.remote_outcome(), RemoteOutcome::Unknown);
        assert_eq!(driver.delivered_outputs(), 1);
        assert_eq!(driver.run(&mut sink).await.unwrap(), ExecuteStatus::Ok);
        assert_eq!(wire.lock().unwrap().sent.len(), 2);
    }

    #[tokio::test]
    async fn driver_output_limits_preserve_partial_without_delivering_over_budget_records() {
        let bytes = serde_json::to_vec(&stream("first").content).unwrap().len();
        for protocol in [
            ProtocolLimits { outputs: 1, ..ProtocolLimits::default() },
            ProtocolLimits { output_bytes: bytes, ..ProtocolLimits::default() },
        ] {
            let wire = driver_wire(vec![stream("first"), stream("second"), terminal("ok")]);
            let mut driver = managed_driver(
                &wire, DriverLimits { protocol, ..DriverLimits::default() }, driver_deadline(),
            );
            let failure = driver.run(&mut driver_sink(&wire)).await.unwrap_err();
            assert_eq!(failure.cause, NativeError::Limit);
            assert_eq!(failure.remote, RemoteOutcome::Unknown);
            assert_eq!(driver.observer().outputs().len(), 1);
            assert_eq!(driver.delivered_outputs(), 1);
            assert_eq!(wire.lock().unwrap().writes.len(), 1);
            assert_eq!(wire.lock().unwrap().reads, 2);
        }
    }

    #[tokio::test]
    async fn driver_failed_or_dropped_cancel_send_is_never_retried_or_acknowledged() {
        for pending in [false, true] {
            let wire = driver_wire(vec![]);
            let mut driver = managed_driver(&wire, DriverLimits::default(), driver_deadline());
            driver.submit().await.unwrap();
            {
                let mut wire = wire.lock().unwrap();
                wire.pending_send = pending;
                wire.send_error = (!pending).then_some(NativeError::OutcomeUnknown);
            }
            if pending {
                let mut cancel = Box::pin(driver.cancel(MessageId::new("cancel-control").unwrap()));
                assert_driver_pending(cancel.as_mut());
                drop(cancel);
                assert_eq!(driver.state(), DriverState::Interrupted);
            } else {
                let failure = driver.cancel(MessageId::new("cancel-control").unwrap()).await.unwrap_err();
                assert_eq!(failure.cause, NativeError::OutcomeUnknown);
                assert_eq!(failure.remote, RemoteOutcome::Unknown);
            }
            assert!(driver.cancellation_attempted());
            assert_eq!(driver.observer().cancellation_state(), CancellationState::NotRequested);
            assert!(driver.observer().cancellation_reply().is_none());
            assert!(driver.cancel(MessageId::new("replacement-control").unwrap()).await.is_err());
            assert_eq!(wire.lock().unwrap().sent.len(), 2);
        }
    }
}
