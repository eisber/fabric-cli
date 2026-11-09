// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Feature-gated, create-only definition copy over already resolved Fabric items.

use super::{FamilyEvidence, FamilyOutput, render_outputs};
use crate::{errors, session::ReadSession};
use azure_core::http::Method;
use fabric_cli_context::{Element, NodeKind, PathElement, PathKind};
use fabric_cli_mutations::{MutationError, MutationUi, NoticeKind, frontdoor::ResourceCommand};
use fabric_cli_output::{
    OutputInput, Timestamp,
    text::{self, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_deploy::definitions::{
    CopyDefinitionDestination, DefinitionCopyError, DefinitionCopyErrorKind, DefinitionCopyReceipt,
    DefinitionError, DefinitionErrorKind, DefinitionLimits, DefinitionTarget,
    prepare_definition_copy, read_copy_definition,
};
use fabric_sdk_http::{ExecutionError, ExecutionFuture, HttpExecutor, PublicOrigin};
use fabric_sdk_operations::{
    OperationError, OperationErrorKind, OperationExecutionError, OperationExecutor,
    OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use futures_util::{FutureExt, select_biased};
use serde_json::json;
use std::{future::Future, sync::Arc, time::Duration};

const WARNING: &str =
    "Definition copy does not copy physical data, sensitivity labels, permissions or credentials.";

/// Original definition-copy outcome, independent of receipt delivery.
#[derive(Debug)]
pub enum DefinitionCopyResult {
    /// Confirmation was declined; no definition was retrieved or item created.
    Declined,
    /// The provider response distinguishes a created item from accepted work.
    Submitted(DefinitionCopyReceipt),
}

/// Original failure, retained without discarding operation or mutation evidence.
#[derive(Debug)]
pub enum DefinitionCopyFailure {
    /// Unsupported route/options or invalid resolved endpoint.
    Validation(MutationError),
    /// Prompt input or delivery failed, rather than a user declining.
    Confirmation(MutationError),
    /// Caller cancellation between phases, including original session evidence.
    Interrupted(OperationError),
    /// Source definition failure, including denied or accepted/unknown reads.
    Read(DefinitionError),
    /// Preparation/create failure, including response, IDs and uncertainty.
    Create(DefinitionCopyError),
}

/// Select Fabric-source copies only; physical and move routes remain unchanged.
pub(super) fn handles(command: &ResourceCommand, source: &PathElement) -> bool {
    matches!(command, ResourceCommand::Copy { .. }) && matches!(source.kind(), PathKind::Fabric(_))
}

struct Endpoints<'a> {
    source: DefinitionTarget<'a>,
    destination: CopyDefinitionDestination<'a>,
    force: bool,
}

fn workspace(element: &Element) -> Option<&Element> {
    let mut current = element;
    loop {
        if matches!(current.kind(), NodeKind::Workspace(_)) {
            return Some(current);
        }
        current = current.parent()?.as_ref();
    }
}

fn validate<'a>(
    command: &ResourceCommand,
    source: &'a PathElement,
    destination: &'a PathElement,
) -> Result<Endpoints<'a>, DefinitionCopyFailure> {
    let invalid =
        |code, message| DefinitionCopyFailure::Validation(MutationError::new(code, message));
    let ResourceCommand::Copy {
        force,
        recursive,
        block_on_path_collision,
        ..
    } = command
    else {
        return Err(invalid("NotSupported", "Only definition copy is supported"));
    };
    if *recursive || *block_on_path_collision || command.args().skill.is_some() {
        return Err(invalid(
            "NotSupported",
            "Definition copy does not support recursive, collision-policy or skill options",
        ));
    }
    let (Some(source), Some(destination)) = (source.fabric(), destination.fabric()) else {
        return Err(invalid(
            "NotSupported",
            "Definition copy requires an explicit Fabric item destination",
        ));
    };
    let (NodeKind::Item(source_type), NodeKind::Item(destination_type)) =
        (source.kind(), destination.kind())
    else {
        return Err(invalid(
            "NotSupported",
            "Definition copy requires an explicit item name, not a workspace or folder",
        ));
    };
    let item_type = source_type.as_str();
    if !matches!(item_type, "Notebook" | "SemanticModel") || source_type != destination_type {
        return Err(invalid(
            "NotSupported",
            "Definition copy requires matching Notebook or SemanticModel item types",
        ));
    }
    if destination.id().is_some() {
        return Err(invalid(
            "AlreadyExists",
            "Definition copy never replaces an existing item, including with force",
        ));
    }
    let source_id = source
        .id()
        .ok_or_else(|| invalid("InvalidInput", "Definition copy source must exist"))?;
    let source_workspace = workspace(source)
        .and_then(Element::id)
        .ok_or_else(|| invalid("InvalidInput", "Source workspace must exist"))?;
    let destination_workspace = workspace(destination)
        .and_then(Element::id)
        .ok_or_else(|| invalid("InvalidInput", "Destination workspace must exist"))?;
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("InvalidInput", "Destination parent must exist"))?;
    let folder_id = if parent.kind() == NodeKind::Folder {
        Some(
            parent
                .id()
                .ok_or_else(|| invalid("InvalidInput", "Destination folder must exist"))?,
        )
    } else {
        None
    };
    if [source_id, source_workspace, destination_workspace]
        .into_iter()
        .chain(folder_id)
        .any(|id| {
            id.len() != 36
                || !id.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
        })
    {
        return Err(invalid(
            "InvalidInput",
            "Copy endpoints require valid scoped GUIDs",
        ));
    }
    if destination.short_name().trim().is_empty()
        || destination.short_name().contains(['/', '\\'])
        || destination.short_name().chars().any(char::is_control)
        || destination.short_name().len() > DefinitionLimits::default().max_path_bytes.get()
    {
        return Err(invalid(
            "InvalidInput",
            "Invalid explicit destination item name",
        ));
    }
    Ok(Endpoints {
        source: DefinitionTarget {
            workspace_id: source_workspace,
            item_id: source_id,
            item_type,
        },
        destination: CopyDefinitionDestination {
            workspace_id: destination_workspace,
            display_name: destination.short_name(),
            item_type,
            folder_id,
        },
        force: *force,
    })
}

/// Execute without a second resolver, a new credential selection or cache writes.
pub(super) async fn execute(
    reads: &ReadSession<'_>,
    http: &HttpExecutor,
    ui: &mut dyn MutationUi,
    command: &ResourceCommand,
    source: &Arc<PathElement>,
    destination: &Arc<PathElement>,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<DefinitionCopyResult, DefinitionCopyFailure> {
    let cancelled = std::pin::pin!(cancelled);
    let cancelled = cancelled.shared();
    let mut source_session = OperationSession::with_context(
        reads.sdk.context(),
        CopyHttp(http),
        OperationLimits::default(),
        cancelled.clone(),
    );
    source_session
        .check_cancelled()
        .await
        .map_err(DefinitionCopyFailure::Interrupted)?;
    let endpoints = validate(command, source, destination)?;
    ui.notice(NoticeKind::Warning, WARNING);
    if !endpoints.force {
        let cancellation = cancelled.clone().fuse();
        let confirmation = async { ui.confirm_result(WARNING).await }.fuse();
        futures_util::pin_mut!(cancellation, confirmation);
        let confirmed = select_biased! {
            () = cancellation => {
                return Err(DefinitionCopyFailure::Interrupted(
                    source_session.evidence(OperationErrorKind::Cancelled),
                ));
            },
            result = confirmation => result.map_err(DefinitionCopyFailure::Confirmation)?,
        };
        source_session
            .check_cancelled()
            .await
            .map_err(DefinitionCopyFailure::Interrupted)?;
        if !confirmed {
            return Ok(DefinitionCopyResult::Declined);
        }
    }
    source_session
        .check_cancelled()
        .await
        .map_err(DefinitionCopyFailure::Interrupted)?;
    let definition = read_copy_definition(&mut source_session, endpoints.source)
        .await
        .map_err(DefinitionCopyFailure::Read)?;
    source_session
        .check_cancelled()
        .await
        .map_err(DefinitionCopyFailure::Interrupted)?;
    let prepared = prepare_definition_copy(
        &definition,
        endpoints.source.item_type,
        endpoints.destination,
        DefinitionLimits::default(),
    )
    .map_err(DefinitionCopyFailure::Create)?;
    // getDefinition itself can be a POST/LRO; create needs independent evidence.
    let mut destination_session = OperationSession::with_context(
        reads.sdk.context(),
        CopyHttp(http),
        OperationLimits::default(),
        cancelled,
    );
    destination_session
        .check_cancelled()
        .await
        .map_err(DefinitionCopyFailure::Interrupted)?;
    prepared
        .execute(&mut destination_session)
        .await
        .map(DefinitionCopyResult::Submitted)
        .map_err(DefinitionCopyFailure::Create)
}

fn output(result: &Result<DefinitionCopyResult, DefinitionCopyFailure>) -> OutputInput {
    match result {
        Ok(DefinitionCopyResult::Declined) => OutputInput {
            message: Some("Definition copy declined; no item was created".into()),
            ..OutputInput::default()
        },
        Ok(DefinitionCopyResult::Submitted(receipt)) => OutputInput {
            message: Some(
                if receipt.response.status == 201 {
                    "Definition created; physical data and sensitivity labels were not copied"
                } else {
                    "Definition copy accepted; creation is not confirmed complete"
                }
                .into(),
            ),
            data: Some(json!({
                "state": if receipt.response.status == 201 { "created" } else { "accepted" },
                "itemId": receipt.item_id,
                "operationId": receipt.operation_id,
                "httpStatus": receipt.response.status,
                "platformOmitted": receipt.platform_omitted,
            })),
            ..OutputInput::default()
        },
        Err(
            DefinitionCopyFailure::Validation(error) | DefinitionCopyFailure::Confirmation(error),
        ) => error.output(),
        Err(DefinitionCopyFailure::Interrupted(error)) => {
            MutationError::from(error.resource_error()).output()
        }
        Err(DefinitionCopyFailure::Read(error)) => match &error.kind {
            DefinitionErrorKind::Operation(error) => {
                MutationError::from(error.resource_error()).output()
            }
            DefinitionErrorKind::DefinitionAccessDenied(_) => {
                MutationError::new("AccessDenied", "Source definition access was denied").output()
            }
            _ => MutationError::new(
                "InvalidDefinition",
                "Source definition could not be copied; inspect retained definition evidence",
            )
            .output(),
        },
        Err(DefinitionCopyFailure::Create(error)) => {
            let code = match error.kind {
                DefinitionCopyErrorKind::UnsupportedType => "NotSupported",
                DefinitionCopyErrorKind::InvalidInput | DefinitionCopyErrorKind::TypeMismatch => {
                    "InvalidInput"
                }
                DefinitionCopyErrorKind::DefinitionAccessDenied(_) => "AccessDenied",
                DefinitionCopyErrorKind::InvalidDefinition => "InvalidDefinition",
                DefinitionCopyErrorKind::Limit => "Limit",
                DefinitionCopyErrorKind::InvalidResponse
                | DefinitionCopyErrorKind::AcceptedWithoutOperation => "OutcomeUnknown",
                DefinitionCopyErrorKind::Operation
                | DefinitionCopyErrorKind::UnexpectedStatus(_) => "DefinitionCopyFailed",
            };
            let mut output = if let Some(operation) = &error.operation
                && error.kind == DefinitionCopyErrorKind::Operation
            {
                MutationError::from(operation.resource_error()).output()
            } else {
                MutationError::new(
                    code,
                    "Definition copy failed or is incomplete; inspect retained create evidence",
                )
                .output()
            };
            output.data = Some(json!({
                "httpStatus": error.response.as_ref().map(|response| response.status),
                "operationId": error.operation.as_ref().and_then(|operation| operation.operation_id.as_deref()),
                "mutationMayHaveOccurred": error.operation.as_ref().is_some_and(|operation| operation.mutation_may_have_occurred),
            }));
            output
        }
    }
}

pub(super) fn render(
    result: Result<DefinitionCopyResult, DefinitionCopyFailure>,
    timestamp: Timestamp,
    command: &str,
    format: OutputFormat,
) -> Result<FamilyOutput, TextError> {
    render_with(result, timestamp.clone(), command, |outputs| {
        render_outputs(outputs, command, timestamp, format, false)
    })
}

fn render_with(
    result: Result<DefinitionCopyResult, DefinitionCopyFailure>,
    timestamp: Timestamp,
    command: &str,
    render: impl FnOnce(Vec<OutputInput>) -> Result<TextPlan, TextError>,
) -> Result<FamilyOutput, TextError> {
    match render(vec![output(&result)]) {
        Ok(plan) => Ok(FamilyOutput {
            plan,
            evidence: FamilyEvidence::DefinitionCopy(result),
        }),
        Err(error) => {
            let mut plan = error.partial_output().cloned().unwrap_or(TextPlan {
                records: Vec::new(),
                exit_code: 1,
            });
            let diagnostic = text::render(
                &errors::error("OutputError", "Rendering definition-copy output failed")
                    .envelope(timestamp, Some(command.into())),
                OutputFormat::Json,
                &TextOptions::default(),
                &text::ascii_width,
                Presentation::default(),
            )?;
            plan.records.extend(diagnostic.records);
            plan.exit_code = 1;
            Ok(FamilyOutput {
                plan,
                evidence: FamilyEvidence::DefinitionCopyRenderFailure { result, error },
            })
        }
    }
}

struct CopyHttp<'a>(&'a HttpExecutor);

fn operation_error(error: ExecutionError) -> OperationExecutionError {
    match error {
        ExecutionError::TimedOut => OperationExecutionError::TimedOut,
        ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            OperationExecutionError::Failed
        }
    }
}

impl OperationExecutor for CopyHttp<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => Method::Get,
                ResourceMethod::Post => Method::Post,
                ResourceMethod::Patch => Method::Patch,
                ResourceMethod::Delete => Method::Delete,
            };
            self.0
                .execute_bound(
                    PublicOrigin::Fabric,
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    &[(
                        "content-type".into(),
                        "application/json; charset=utf-8".into(),
                    )],
                    request.max_response_bytes(),
                )
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(operation_error)
        })
    }

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.0.wait_for(delay).await.map_err(operation_error) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_cli_context::{ItemType, WorkspaceType};
    use fabric_cli_mutations::frontdoor::ResourceArgs;

    #[test]
    fn folder_destination_is_rejected_and_moves_are_never_definition_copies() {
        let tenant = Element::tenant("Tenant", "tenant");
        let workspace = Element::child(
            "Demo",
            Some("11111111-1111-1111-1111-111111111111".into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            tenant,
        )
        .unwrap();
        let source = PathElement::from_fabric(
            Element::child(
                "Book",
                Some("22222222-2222-2222-2222-222222222222".into()),
                NodeKind::Item(ItemType::parse("Notebook").unwrap()),
                workspace.clone(),
            )
            .unwrap(),
        );
        let destination = PathElement::from_fabric(
            Element::child(
                "Folder",
                Some("55555555-5555-5555-5555-555555555555".into()),
                NodeKind::Folder,
                workspace,
            )
            .unwrap(),
        );
        let args = ResourceArgs {
            path: vec!["Demo.Workspace/Book.Notebook".into()],
            output_format: None,
            skill: None,
        };
        let copy = ResourceCommand::Copy {
            args: args.clone(),
            destination: vec!["Demo.Workspace/Folder.Folder".into()],
            force: true,
            recursive: false,
            block_on_path_collision: false,
        };
        assert!(handles(&copy, &source));
        assert!(matches!(
            validate(&copy, &source, &destination),
            Err(DefinitionCopyFailure::Validation(error)) if error.code == "NotSupported"
        ));
        let moving = ResourceCommand::Move {
            args,
            destination: vec!["Demo.Workspace/Folder.Folder".into()],
            force: true,
            recursive: false,
        };
        assert!(!handles(&moving, &source));
    }

    #[test]
    fn renderer_failure_preserves_created_response_ids_and_prior_output() {
        let receipt = DefinitionCopyReceipt {
            response: OperationResponse {
                status: 201,
                body: br#"{"id":"33333333-3333-3333-3333-333333333333"}"#.to_vec(),
                headers: Default::default(),
            },
            item_id: Some("33333333-3333-3333-3333-333333333333".into()),
            operation_id: None,
            warnings: Vec::new(),
            platform_omitted: true,
        };
        let result = render_with(
            Ok(DefinitionCopyResult::Submitted(receipt)),
            Timestamp::parse("2026-11-05T12:00:00Z").unwrap(),
            "cp",
            |_| {
                Err(TextError::AfterOutput {
                    error: Box::new(TextError::InvalidTableEntries),
                    partial: Box::new(text::render_line(
                        "prior warning",
                        text::Style::Grey,
                        fabric_cli_output::OutputStream::Stderr,
                        Presentation::default(),
                    )),
                })
            },
        )
        .unwrap();
        assert_eq!(result.plan.exit_code, 1);
        assert!(matches!(
            &result.evidence,
            FamilyEvidence::DefinitionCopyRenderFailure {
                result: Ok(DefinitionCopyResult::Submitted(receipt)),
                ..
            } if receipt.response.status == 201
                && receipt.item_id.as_deref() == Some("33333333-3333-3333-3333-333333333333")
                && receipt.platform_omitted
        ));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(result.write_to(&mut stdout, &mut stderr).unwrap(), 1);
        assert!(String::from_utf8(stderr).unwrap().contains("prior warning"));
        assert!(String::from_utf8(stdout).unwrap().contains("OutputError"));
    }
}
