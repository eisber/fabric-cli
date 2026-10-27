// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TABLE composition over the same read snapshot, HTTP, OneLake and job engines.

use crate::{
    errors,
    families::{FamilyEvidence, FamilyOutput, context::CommandContext},
    jobs::CheckedJobExecutor,
    session::ReadSession,
};
use azure_core::http::Method;
use fabric_cli_context::{NodeKind, PathInput, PathKind, ResolveOptions};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputStream, RenderPlan,
    text::{self, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_cli_tables::{
    FabricLoadCompletion, ResolvedTablePath, SharedTableExecutor, StorageReads, TableCommand,
    TableFailure, TableHost, TableOperationObserver, TableOutput, TableResolver,
};
use fabric_sdk_http::{ExecutionError, HttpExecutor, PublicOrigin};
use fabric_sdk_jobs::{JobClient, SharedJobOperations};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_operations::OperationWarning;
use fabric_sdk_tables::{
    JobMaintenance, TableClient, TableError, TableErrorKind, TableFuture, TablePath, TableRequest,
    TableResponse, TableWithOneLake,
};
use serde_json::{Map, Value};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

/// Retained table outcome and shared-operation observations, independent of delivery.
#[derive(Debug)]
pub struct TableEvidence {
    /// Original successful result or typed failure, including mutation uncertainty.
    pub result: Result<TableOutput, TableFailure>,
    /// Shared operation warnings, retained even when debug presentation is disabled.
    pub warnings: Vec<OperationWarning>,
}

/// Explicit central TABLE capabilities; no runtime, network or identity discovery.
pub struct TableSession<'a, 'session> {
    /// Same command-local metadata, selected credential, clock and context as reads.
    pub reads: &'a ReadSession<'session>,
    /// Same HTTP executor and asynchronous timer as the other remote families.
    pub http: &'a HttpExecutor,
    /// Actual non-secret configuration snapshot.
    pub settings: &'a Map<String, Value>,
}

impl TableSession<'_, '_> {
    /// Execute a parsed root TABLE leaf through the production library/HTTP path.
    pub async fn dispatch(
        &self,
        command: &TableCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<FamilyOutput, TextError> {
        let format = OutputFormat::resolve(
            command.output_format.as_deref(),
            self.settings
                .get("output_format")
                .and_then(Value::as_str)
                .or(Some("text")),
        )?;
        let warnings = Warnings::default();
        let mut diagnostics = Vec::new();
        let result = async {
            if self.reads.tenant.id() != Some(self.reads.sdk.selection().tenant())
                || self.reads.tenant.kind() != NodeKind::Tenant
                || self.reads.sdk.selection().audience() != fabric_sdk::Audience::Fabric
                || !self.reads.current_path.starts_with('/')
            {
                return Err(errors::error("InvalidInput", "Invalid selected read context").into());
            }
            let storage_selection = fabric_sdk::CredentialSelection::new(
                self.reads.sdk.selection().account(),
                self.reads.sdk.selection().tenant(),
                fabric_sdk::Audience::OneLake,
            )
            .map_err(|_| errors::error("InvalidInput", "Invalid selected storage context"))?;
            let sdk = TableClient::new(
                self.reads.sdk.credential().clone(),
                self.reads.sdk.selection(),
                &storage_selection,
            )?;
            let raw = StorageHttp(self.http);
            let completion = FabricLoadCompletion {
                http: self.http,
                limits: Default::default(),
                observer: &warnings,
            };
            let loads = SharedTableExecutor {
                http: self.http,
                storage: &raw,
                operations: &completion,
            };
            let storage = OneLakeHttpExecutor::new(Arc::new(self.http.clone()));
            let executor = TableWithOneLake {
                tables: &loads,
                storage: &storage,
            };
            let jobs_http = Arc::new(CheckedJobExecutor(Arc::new(
                fabric_cli_jobs::HttpJobExecutor::new(self.http.clone()),
            )));
            let jobs = JobClient::new(
                self.reads.sdk.credential().clone(),
                self.reads.sdk.selection().clone(),
                jobs_http.clone(),
                Default::default(),
            );
            let operations = SharedJobOperations::from_job_executor(
                self.reads.sdk.credential().clone(),
                self.reads.sdk.selection().clone(),
                jobs_http,
                Default::default(),
            );
            let maintenance = JobMaintenance {
                jobs: &jobs,
                wait: Default::default(),
                cancel_on_timeout: self
                    .settings
                    .get("job_cancel_ontimeout")
                    .and_then(Value::as_str)
                    != Some("false"),
                operations: Some(&operations),
            };
            let resolver = Targets {
                reads: self.reads,
                http: self.http,
                context: tokio::sync::Mutex::new(CommandContext::default()),
            };
            TableHost {
                sdk: &sdk,
                executor: &executor,
                jobs: &maintenance,
                resolver: &resolver,
                output_format: "json",
                text: None,
                timestamp: self.reads.timestamp.clone(),
                limits: Default::default(),
            }
            .execute_observed(command, cancelled, &mut diagnostics)
            .await
            .expect("Vec diagnostic writes cannot fail")
        }
        .await;
        let warnings = warnings.0.into_inner().expect("warning mutex poisoned");
        let mut plan = TextPlan {
            records: Vec::new(),
            exit_code: 0,
        };
        if !diagnostics.is_empty() {
            plan.records.push(RenderPlan {
                stream: OutputStream::Stderr,
                exit_code: 0,
                payload: diagnostics,
            });
        }
        if self.settings.get("debug_enabled").and_then(Value::as_str) == Some("true") {
            for warning in &warnings {
                match warning {
                    OperationWarning::InvalidRetryAfter => plan.records.extend(
                        text::render_notice(
                            text::Notice::Warning(
                                "Invalid Retry-After; using the shared operation fallback",
                            ),
                            None,
                            Presentation::default(),
                        )
                        .records,
                    ),
                }
            }
        }
        let rendered = match &result {
            Ok(output) => output
                .input
                .as_ref()
                .map(|input| {
                    text::render(
                        &OutputEnvelope::new(self.reads.timestamp.clone(), input.clone()),
                        format,
                        &TextOptions {
                            show_headers: output.show_headers,
                            key_value: output.show_key_value_list,
                            ..Default::default()
                        },
                        &text::ascii_width,
                        Presentation::default(),
                    )
                })
                .transpose(),
            Err(error) => text::render(
                &error.envelope(
                    self.reads.timestamp.clone(),
                    Some(command.leaf_name().into()),
                ),
                format,
                &TextOptions::default(),
                &text::ascii_width,
                Presentation::default(),
            )
            .map(Some),
        };
        match rendered {
            Ok(Some(rendered)) => {
                plan.exit_code = rendered.exit_code;
                plan.records.extend(rendered.records);
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(partial) = error.partial_output() {
                    plan.records.extend(partial.records.clone());
                }
                return Err(TextError::AfterOutput {
                    error: Box::new(error),
                    partial: Box::new(plan),
                });
            }
        }
        Ok(FamilyOutput {
            plan,
            evidence: FamilyEvidence::Table(TableEvidence { result, warnings }),
        })
    }
}

struct Targets<'a, 'session> {
    reads: &'a ReadSession<'session>,
    http: &'a HttpExecutor,
    context: tokio::sync::Mutex<CommandContext>,
}

impl TableResolver for Targets<'_, '_> {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        must_exist: bool,
        mut cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>> {
        Box::pin(async move {
            let mut context = self.context.lock().await;
            let current = context
                .resolve(
                    self.reads,
                    self.http,
                    self.reads.tenant.clone(),
                    self.reads.current_path,
                    ResolveOptions::default(),
                    cancelled.as_mut(),
                )
                .await?;
            let tokens = path.iter().map(String::as_str).collect::<Vec<_>>();
            let element = context
                .resolve_path(
                    self.reads,
                    self.http,
                    current,
                    PathInput::Tokens(&tokens),
                    ResolveOptions {
                        raise_error: must_exist,
                        supports_local_path: false,
                    },
                    cancelled.as_mut(),
                )
                .await?;
            if !matches!(element.kind(), PathKind::OneLake(_)) {
                return Err(errors::error(
                    "InvalidPath",
                    "TABLE requires a resolved OneLake path",
                ));
            }
            let mut parent = element.parent();
            let mut item = None;
            while let Some(node) = parent {
                if let Some(fabric) = node.fabric()
                    && let NodeKind::Item(kind) = fabric.kind()
                {
                    item = Some((fabric, kind));
                    break;
                }
                parent = node.parent();
            }
            let (item, item_type) = item
                .ok_or_else(|| errors::error("InvalidPath", "OneLake path has no owning item"))?;
            let workspace = item
                .workspace()
                .and_then(|workspace| workspace.id().map(str::to_owned))
                .ok_or_else(|| errors::error("InvalidPath", "OneLake path has no workspace ID"))?;
            let id = item
                .id()
                .ok_or_else(|| errors::error("NotFound", "OneLake path has no owning item ID"))?;
            let local = element
                .local_path()
                .ok_or_else(|| errors::error("InvalidPath", "TABLE requires a OneLake child"))?;
            Ok(ResolvedTablePath {
                path: TablePath::new(workspace, id, local)
                    .map_err(|_| errors::error("InvalidPath", "Invalid resolved TABLE path"))?,
                item_type,
            })
        })
    }
}

#[derive(Default)]
struct Warnings(Mutex<Vec<OperationWarning>>);
impl TableOperationObserver for Warnings {
    fn warning(&self, warning: OperationWarning) -> Result<(), TableError> {
        self.0.lock().expect("warning mutex poisoned").push(warning);
        Ok(())
    }
}

struct StorageHttp<'a>(&'a HttpExecutor);
impl StorageReads for StorageHttp<'_> {
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async move {
            let method = match request.method() {
                "GET" => Method::Get,
                "HEAD" => Method::Head,
                _ => return Err(TableError::new(TableErrorKind::InvalidInput)),
            };
            self.0
                .execute_bound(
                    PublicOrigin::OneLake,
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    &[],
                    request.max_response_bytes(),
                )
                .await
                .map(|response| TableResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(|error| {
                    TableError::new(match error {
                        ExecutionError::TimedOut => TableErrorKind::TimedOut,
                        ExecutionError::ResponseLimit => TableErrorKind::Limit,
                        ExecutionError::InvalidInput
                        | ExecutionError::Transport
                        | ExecutionError::RetriesExhausted
                        | ExecutionError::RetryDelay => TableErrorKind::Execution,
                    })
                })
        })
    }
}
