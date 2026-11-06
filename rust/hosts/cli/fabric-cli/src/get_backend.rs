// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Concrete get composition over the shared SDK's native read capabilities.

use crate::{
    get_budget::GetReadBudget,
    get_command::{
        GetAcquireOptions, GetBackend, GetCommandError, GetFormat, GetFuture, GetLimits,
        GetPayload, GetTargetKind, ResolvedGet,
    },
    get_resolution::{ResolvedGetTarget, resolve},
    get_target::{GetRepresentation, GetTarget},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Client,
    documents::{
        Coverage, CoverageReasonKind, DocsErrorKind, DocsKey, DocsOptions, DocsOwner,
        DocumentsProvider, NodeStatus,
    },
    items::ItemExecutor,
    metadata::{MetadataHost, MetadataLimits, MetadataTarget},
    observe::{ObserveOptions, ReadHandle, observe},
    representations::{DefinitionRead, DefinitionTarget, RepresentationLimits},
    workspaces::WorkspaceExecutor,
};
use fabric_sdk_onelake::{OneLakeClient, OneLakeExecutor, OneLakePath};
use fabric_sdk_operations::OperationExecutor;
use serde_json::{Value, json};
use std::{future::pending, num::NonZeroUsize, time::Duration};

/// Existing service dependencies and optional explicitly admitted providers.
///
/// Construction neither authenticates nor installs a transport/runtime. The
/// same SDK operations are available directly to non-CLI consumers.
pub struct SdkGetBackend<'a> {
    /// Explicit Fabric client and credential/endpoint binding.
    pub client: &'a Client,
    /// Existing generic item reader.
    pub items: &'a dyn ItemExecutor,
    /// Existing workspace reader.
    pub workspaces: &'a dyn WorkspaceExecutor,
    /// Existing shared operation transport/time policy.
    pub operations: &'a dyn OperationExecutor,
    /// Explicit storage client/executor, if file reads are enabled.
    pub storage: Option<(&'a OneLakeClient, &'a dyn OneLakeExecutor)>,
    /// An admitted docs provider; none never activates a private service.
    pub documents: Option<&'a dyn DocumentsProvider>,
}

/// Resolved operation and its invocation-wide read allowance.
pub struct SdkGetTarget<'a> {
    target: ResolvedGetTarget,
    budget: GetReadBudget<'a>,
}

fn backend_error(
    code: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> GetCommandError {
    GetCommandError::Backend {
        code,
        source: Box::new(source),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct CapabilityError(&'static str);

fn unsupported(message: &'static str) -> GetCommandError {
    backend_error("UnsupportedCapability", CapabilityError(message))
}

impl<'host> GetBackend for SdkGetBackend<'host> {
    type Target = SdkGetTarget<'host>;

    fn resolve<'a>(
        &'a self,
        path: &'a str,
        limits: GetLimits,
    ) -> GetFuture<'a, ResolvedGet<Self::Target>> {
        Box::pin(async move {
            let parsed =
                GetTarget::parse(path).map_err(|error| backend_error("InvalidTarget", error))?;
            let (kind, formats, default_format, recursive, wait) = match &parsed {
                GetTarget::Workspace { .. }
                | GetTarget::Item {
                    representation: GetRepresentation::Metadata,
                    ..
                } => (
                    GetTargetKind::Metadata,
                    vec![GetFormat::Json],
                    GetFormat::Json,
                    false,
                    false,
                ),
                GetTarget::Operation { result, .. } => (
                    GetTargetKind::ReturnedHandle,
                    if *result {
                        vec![GetFormat::Json, GetFormat::Raw]
                    } else {
                        vec![GetFormat::Json]
                    },
                    GetFormat::Json,
                    false,
                    true,
                ),
                GetTarget::Item { representation, .. } => match representation {
                    GetRepresentation::Definition { part, .. } => (
                        GetTargetKind::Representation,
                        if part.is_some() {
                            vec![GetFormat::Json, GetFormat::Raw]
                        } else {
                            vec![GetFormat::Json]
                        },
                        GetFormat::Json,
                        false,
                        true,
                    ),
                    GetRepresentation::File(_) => {
                        if self.storage.is_none() {
                            return Err(unsupported("no explicit storage provider is configured"));
                        }
                        (
                            GetTargetKind::Representation,
                            vec![GetFormat::Json, GetFormat::Raw],
                            GetFormat::Raw,
                            false,
                            false,
                        )
                    }
                    GetRepresentation::Docs(_) => {
                        if self.documents.is_none() {
                            return Err(unsupported(
                                "no admitted documentation provider is configured",
                            ));
                        }
                        (
                            GetTargetKind::Representation,
                            vec![GetFormat::Json, GetFormat::Markdown],
                            GetFormat::Markdown,
                            true,
                            false,
                        )
                    }
                    GetRepresentation::Job { .. } => (
                        GetTargetKind::ReturnedHandle,
                        vec![GetFormat::Json],
                        GetFormat::Json,
                        false,
                        true,
                    ),
                    GetRepresentation::Metadata => unreachable!("metadata handled above"),
                },
            };
            let budget = GetReadBudget::new(
                self.items,
                self.workspaces,
                self.operations,
                self.storage.map(|(_, executor)| executor),
                limits.max_bytes.get(),
                100,
            );
            let target = resolve(
                parsed,
                self.client,
                &budget,
                &budget,
                limits.max_bytes,
                pending(),
            )
            .await
            .map_err(|error| backend_error("TargetResolutionFailed", error))?;
            Ok(ResolvedGet {
                target: SdkGetTarget { target, budget },
                kind,
                formats,
                default_format,
                recursive,
                wait,
            })
        })
    }

    fn retrieve<'a>(
        &'a self,
        resolved: &'a ResolvedGet<Self::Target>,
        options: GetAcquireOptions,
    ) -> GetFuture<'a, GetPayload> {
        Box::pin(async move {
            let target = &resolved.target.target;
            let budget = &resolved.target.budget;
            let cap = options.limits.max_bytes;
            let remaining =
                NonZeroUsize::new(budget.remaining_bytes()).ok_or(GetCommandError::ByteLimit {
                    limit: cap.get(),
                    observed: cap.get().saturating_add(1),
                })?;
            let source_limits = RepresentationLimits {
                max_response_bytes: remaining,
                max_result_bytes: cap,
            };
            let metadata = MetadataHost {
                client: self.client,
                items: budget,
                workspaces: budget,
            };
            match target {
                ResolvedGetTarget::Workspace(workspace_id) => {
                    let read = metadata
                        .read(
                            MetadataTarget::Workspace { workspace_id },
                            MetadataLimits {
                                max_response_bytes: remaining,
                                max_result_bytes: cap,
                            },
                            pending(),
                        )
                        .await
                        .map_err(|error| backend_error("MetadataReadFailed", error))?;
                    Ok(json_payload(read.data().clone()))
                }
                ResolvedGetTarget::Operation {
                    operation_id,
                    result,
                } => {
                    let observation = observe(
                        self.client,
                        budget,
                        &ReadHandle::Operation {
                            operation_id: operation_id.clone(),
                        },
                        observer_options(options, *result, remaining),
                        pending(),
                    )
                    .await
                    .map_err(|error| backend_error("ObservationFailed", error))?;
                    observation_payload(observation)
                }
                ResolvedGetTarget::Item {
                    workspace_id,
                    item_id,
                    representation,
                } => match representation {
                    GetRepresentation::Metadata => {
                        let read = metadata
                            .read(
                                MetadataTarget::Item {
                                    workspace_id,
                                    item_id,
                                },
                                MetadataLimits {
                                    max_response_bytes: remaining,
                                    max_result_bytes: cap,
                                },
                                pending(),
                            )
                            .await
                            .map_err(|error| backend_error("MetadataReadFailed", error))?;
                        Ok(json_payload(read.data().clone()))
                    }
                    GetRepresentation::Job { job_id } => {
                        let observation = observe(
                            self.client,
                            budget,
                            &ReadHandle::Job {
                                workspace_id: workspace_id.clone(),
                                item_id: item_id.clone(),
                                job_id: job_id.clone(),
                            },
                            observer_options(options, false, remaining),
                            pending(),
                        )
                        .await
                        .map_err(|error| backend_error("ObservationFailed", error))?;
                        observation_payload(observation)
                    }
                    GetRepresentation::Definition {
                        native_format,
                        part,
                    } => {
                        let mut read = fabric_sdk::representations::read_definition(
                            self.client,
                            budget,
                            &DefinitionTarget {
                                workspace_id: workspace_id.clone(),
                                item_id: item_id.clone(),
                                native_format: native_format.clone(),
                            },
                            source_limits,
                            pending(),
                        )
                        .await
                        .map_err(|error| backend_error("DefinitionReadFailed", error))?;
                        if let DefinitionRead::Accepted { operation_id } = &read {
                            if !options.wait {
                                let mut receipt = json_payload(json!({
                                    "status":"accepted",
                                    "handle":format!("/operations/{operation_id}"),
                                    "completed":false
                                }));
                                receipt.data["meta"]["outcome"] = json!("accepted");
                                receipt.complete = false;
                                return Ok(receipt);
                            }
                            let handle = read.accepted_handle().ok_or_else(|| {
                                unsupported("definition did not retain its returned handle")
                            })?;
                            let observation = observe(
                                self.client,
                                budget,
                                &handle,
                                observer_options(options, true, remaining),
                                pending(),
                            )
                            .await
                            .map_err(|error| backend_error("ObservationFailed", error))?;
                            let result = observation
                                .result
                                .ok_or_else(|| unsupported("definition result was not returned"))?;
                            read =
                                DefinitionRead::from_completed_result(&result.bytes, source_limits)
                                    .map_err(|error| {
                                        backend_error("DefinitionResultInvalid", error)
                                    })?;
                        }
                        match part {
                            Some(path) => {
                                let bytes = read.part_bytes(path).map_err(|error| {
                                    backend_error("DefinitionPartFailed", error)
                                })?;
                                Ok(byte_payload(bytes, None))
                            }
                            None => match read {
                                DefinitionRead::Completed { definition } => {
                                    Ok(json_payload(definition))
                                }
                                DefinitionRead::Accepted { .. } => {
                                    unreachable!("acceptance returned above")
                                }
                            },
                        }
                    }
                    GetRepresentation::File(relative) => {
                        let (client, _) = self
                            .storage
                            .ok_or_else(|| unsupported("storage unavailable"))?;
                        let path = OneLakePath::new(workspace_id, item_id, relative)
                            .map_err(|error| backend_error("InvalidStorageTarget", error))?;
                        let file = fabric_sdk::representations::read_file(
                            client,
                            budget,
                            &path,
                            source_limits,
                            pending(),
                        )
                        .await
                        .map_err(|error| backend_error("FileReadFailed", error))?;
                        let media_type = file
                            .headers
                            .iter()
                            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                            .map(|(_, value)| value.clone());
                        Ok(byte_payload(file.bytes, media_type))
                    }
                    GetRepresentation::Docs(view) => {
                        let provider = self
                            .documents
                            .ok_or_else(|| unsupported("documentation unavailable"))?;
                        let root = DocsKey {
                            owner: DocsOwner::Item {
                                tenant: self.client.selection().tenant().into(),
                                workspace: workspace_id.clone(),
                                item: item_id.clone(),
                            },
                            view: if view.is_empty() {
                                "docs".into()
                            } else {
                                format!("docs/{view}")
                            },
                            facet: None,
                            subresource: None,
                            stage: None,
                            revision_selector: None,
                        };
                        let materialized = fabric_sdk::documents::materialize(
                            provider,
                            root,
                            DocsOptions {
                                recursive: options.recursive,
                                depth: if options.recursive {
                                    Some(options.depth.unwrap_or(1))
                                } else {
                                    None
                                },
                                max_nodes: NonZeroUsize::new(100).unwrap(),
                                max_pages: NonZeroUsize::new(10).unwrap(),
                                max_bytes: remaining,
                            },
                            pending(),
                        )
                        .await;
                        let (graph, mut descendant_partial) = match materialized {
                            Ok(graph) => (graph, false),
                            Err(error)
                                if matches!(error.kind, DocsErrorKind::Provider(_))
                                    && error.partial.nodes.iter().any(|node| {
                                        node.key == error.partial.root
                                            && node.status == NodeStatus::Available
                                    }) =>
                            {
                                (*error.partial, true)
                            }
                            Err(error) => return Err(backend_error("DocsReadFailed", error)),
                        };
                        if let Coverage::Incomplete { reasons } = &graph.coverage {
                            descendant_partial |= reasons.iter().any(|reason| {
                                reason.key.as_ref().is_some_and(|key| key != &graph.root)
                                    && matches!(
                                        reason.kind,
                                        CoverageReasonKind::Denied
                                            | CoverageReasonKind::NotFound
                                            | CoverageReasonKind::Unsupported
                                            | CoverageReasonKind::Unavailable
                                    )
                            });
                        }
                        let complete = match &graph.coverage {
                            Coverage::Complete => true,
                            Coverage::Incomplete { reasons } => reasons.iter().all(|reason| {
                                matches!(
                                    reason.kind,
                                    CoverageReasonKind::NonRecursive
                                        | CoverageReasonKind::DepthLimit
                                        | CoverageReasonKind::CrossOwnerBoundary
                                        | CoverageReasonKind::OutsideSubtree
                                        | CoverageReasonKind::Unadvertised
                                        | CoverageReasonKind::ReferenceOnly
                                        | CoverageReasonKind::Omitted
                                ) || (descendant_partial
                                    && matches!(
                                        reason.kind,
                                        CoverageReasonKind::Denied
                                            | CoverageReasonKind::NotFound
                                            | CoverageReasonKind::Unsupported
                                            | CoverageReasonKind::Unavailable
                                    ))
                            }),
                        };
                        let graph_data =
                            serde_json::to_value(&graph).map_err(GetCommandError::Json)?;
                        let data = json!({
                            "data":graph_data,
                            "meta":{
                                "representation":"docs",
                                "outcome":if descendant_partial {"partial"} else {"completed"},
                                "coverage":graph.coverage,"root":graph.root
                            }
                        });
                        Ok(GetPayload {
                            data,
                            raw: None,
                            markdown: None,
                            media_type: "text/markdown".into(),
                            complete,
                        })
                    }
                },
            }
        })
    }

    fn validate_projection(&self, expression: &str) -> Result<(), GetCommandError> {
        fabric_cli_query::validate_projection(expression)
            .map_err(|error| backend_error("InvalidProjection", error))
    }

    fn project(&self, data: &Value, expression: &str) -> Result<Value, GetCommandError> {
        let projected = fabric_cli_query::project_value(&data["data"], expression)
            .map_err(|error| backend_error("InvalidProjection", error))?;
        let mut meta = data["meta"].clone();
        meta["projectionApplied"] = json!(true);
        Ok(json!({"data":projected,"meta":meta}))
    }

    fn render_markdown(
        &self,
        data: &Value,
        max_bytes: NonZeroUsize,
    ) -> Result<String, GetCommandError> {
        if data
            .get("meta")
            .and_then(|meta| meta.get("representation"))
            .and_then(Value::as_str)
            != Some("docs")
        {
            return Err(GetCommandError::MissingRepresentation(GetFormat::Markdown));
        }
        if data["meta"]["projectionApplied"] != true
            && let Ok(graph) =
                serde_json::from_value::<fabric_sdk::documents::GraphResult>(data["data"].clone())
        {
            return graph
                .to_markdown(max_bytes)
                .map_err(|error| backend_error("DocsRenderingFailed", error));
        }
        let header = "# Projected documentation\n\nCoverage and canonical root are retained in `meta`.\n\n    ";
        let remaining =
            max_bytes
                .get()
                .checked_sub(header.len())
                .ok_or(GetCommandError::ByteLimit {
                    limit: max_bytes.get(),
                    observed: header.len(),
                })?;
        let bytes = crate::get_command::bounded_json(data, remaining)?;
        let mut output = String::with_capacity(header.len() + bytes.len());
        output.push_str(header);
        output.push_str(std::str::from_utf8(&bytes).expect("JSON encoding is UTF-8"));
        Ok(output)
    }
}

fn observer_options(options: GetAcquireOptions, result: bool, cap: NonZeroUsize) -> ObserveOptions {
    ObserveOptions {
        wait: options.wait,
        result,
        max_requests: NonZeroUsize::new(100).unwrap(),
        max_response_bytes: cap,
        max_delay: Duration::from_secs(60),
    }
}

fn json_payload(data: Value) -> GetPayload {
    GetPayload {
        data: json!({"data":data,"meta":{"outcome":"completed"}}),
        raw: None,
        markdown: None,
        media_type: "application/json".into(),
        complete: true,
    }
}

fn byte_payload(bytes: Vec<u8>, media_type: Option<String>) -> GetPayload {
    let data = json!({
        "data":{"content":STANDARD.encode(&bytes),"mediaType":media_type,"encoding":"base64"},
        "meta":{"outcome":"completed"}
    });
    GetPayload {
        data,
        raw: Some(bytes),
        markdown: None,
        media_type: media_type.unwrap_or_default(),
        complete: true,
    }
}

fn observation_payload(
    observation: fabric_sdk::observe::Observation,
) -> Result<GetPayload, GetCommandError> {
    match observation.result {
        Some(result) => Ok(byte_payload(result.bytes, result.media_type)),
        None => Ok(json_payload(json!({
            "status":observation.raw_status,
            "terminal":observation.terminal,
            "workSucceeded":observation.work_succeeded,
            "requests":observation.requests
        }))),
    }
}
