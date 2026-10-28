// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Central family composition over the existing command-local metadata session.

pub(crate) mod context;
mod deploy_http;
mod export_files;
pub mod transfer_files;

use crate::{errors, parser::Invocation, session::ReadSession};
use context::CommandContext;
use fabric_cli_context::{
    Element, NodeKind, PathElement, PathInput, PathResolver, ResolveOptions, Resolver,
};
use fabric_cli_local::{ConfigStore, LocalEffects, NavigationHost, NavigationState};
use fabric_cli_mutations::{
    FabricMutationBackend, MutationError, MutationHost, MutationMetadata, MutationResult,
    OneLakeWriteError, OneLakeWriteHost, OneLakeWriteResult,
    frontdoor::{
        ResourceCommand, ResourceConfig, ResourceContext, ResourceFiles, ResourceHost,
        ResourceTransferHost,
    },
};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, Timestamp,
    text::{self, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_cli_resources::{
    FamilyPresentation,
    frontdoor::{self, AccessHost, AccessKind, ApiClients},
};
use fabric_cli_transfer::{
    TransferContext, TransferError, TransferFiles, TransferHost, TransferKind, TransferMetadata,
    TransferResult, TransferUi,
};
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_onelake::{OneLakeClient, OneLakeLimits, OneLakeWriteLimits};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_operations::OperationLimits;
use serde_json::Value;
use std::{
    future::{Future, pending},
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex},
};

/// Original family evidence remains inspectable after rendering or writer failure.
#[derive(Debug)]
pub enum FamilyEvidence {
    /// Local execution has no remote mutation result.
    Local,
    /// Access result retains original accepted-operation uncertainty.
    Access(Result<fabric_cli_resources::CommandValue, fabric_cli_resources::CommandError>),
    /// Fabric mutation result retains earlier creation and cache evidence.
    Resource(Result<MutationResult, MutationError>),
    /// Storage evidence remains distinct from Fabric mutation evidence.
    Storage(Result<OneLakeWriteResult, OneLakeWriteError>),
    /// Definition transfer retains original operation IDs and file progress.
    Transfer(Result<TransferResult, TransferError>),
    /// Config deployment retains actual completed IDs and uncertain operations.
    Deploy(Result<crate::deploy::DeployResult, crate::deploy::DeployError>),
    /// Original table, storage, generic-operation and maintenance-job evidence.
    Table(crate::tables::TableEvidence),
}

/// A rendered result is not yet a delivered result or an applied cache effect.
pub struct FamilyOutput {
    /// Ordered source streams and exit code.
    pub plan: TextPlan,
    /// Original successful or failed operation/file evidence.
    pub evidence: FamilyEvidence,
}

impl FamilyOutput {
    /// Write once; retain this object on failure and never replay the operation.
    pub fn write_to(&self, stdout: &mut impl Write, stderr: &mut impl Write) -> io::Result<u8> {
        fabric_cli_transfer::write_result(&self.plan, stdout, stderr)
    }
}

/// Explicit capabilities for the existing runtime; no ambient discovery.
pub struct FamilySession<'a, 'session> {
    /// Same Fabric credential, clock and async metadata session as reads/jobs.
    pub reads: &'a ReadSession<'session>,
    /// Same shared HTTP executor, not another transport or retry stack.
    pub http: &'a HttpExecutor,
    /// Explicit non-secret configuration store.
    pub store: &'a ConfigStore,
    /// Actual mutable navigation state, owned by this caller.
    pub navigation: &'a mut NavigationState,
    /// Actual cache/browser/terminal capabilities.
    pub local: &'a mut dyn LocalEffects,
    /// Existing resource notice/confirmation policy.
    pub mutations: &'a mut dyn fabric_cli_mutations::MutationUi,
    /// Existing access confirmation policy; absent means unsupported, not consent.
    pub confirmation: Option<&'a mut dyn fabric_cli_resources::Confirmation>,
    /// Definition progress, warnings and confirmation policy.
    pub transfer_ui: &'a dyn TransferUi,
    /// Stable caller-owned file root/policy; absent rejects only file transfers.
    pub transfer_files: Option<&'a mut dyn TransferFiles>,
    /// Bounded caller-classified local file input for storage copies.
    pub resource_files: Option<&'a mut dyn ResourceFiles>,
}

impl FamilySession<'_, '_> {
    /// Dispatch an actual root invocation through its released production API.
    ///
    /// Metadata is hydrated asynchronously before pure lookup. Remote family
    /// failures remain typed; callers keep this evidence even if output fails.
    pub async fn dispatch(
        &mut self,
        invocation: &Invocation,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<FamilyOutput, TextError> {
        let mut cancelled = std::pin::pin!(cancelled);
        if self.reads.tenant.id() != Some(self.reads.sdk.selection().tenant())
            || self.reads.tenant.kind() != NodeKind::Tenant
            || self.reads.sdk.selection().audience() != fabric_sdk::Audience::Fabric
            || !self.reads.current_path.starts_with('/')
        {
            return self.command_failure(
                errors::error("InvalidInput", "Invalid selected read context"),
                invocation,
                OutputFormat::Json,
            );
        }
        let settings = match self.store.read() {
            Ok(settings) => settings,
            Err(error) => {
                return self.command_failure(error.command_error(), invocation, OutputFormat::Json);
            }
        };
        let configured = settings.get("output_format").and_then(Value::as_str);
        if let Invocation::Table(command) = invocation {
            return crate::tables::TableSession {
                reads: self.reads,
                http: self.http,
                settings: &settings,
            }
            .dispatch(command, cancelled.as_mut())
            .await;
        }
        let explicit = match invocation {
            Invocation::Local(command) => command.output_format.as_deref(),
            Invocation::Resource(command) => command.args().output_format.as_deref(),
            Invocation::Transfer(command) => command.output_format.as_deref(),
            Invocation::Deploy(command) => command.output_format.as_deref(),
            _ => None,
        };
        let format = match invocation {
            Invocation::Access(command) if command.output_format.is_some() => {
                command.output_format.expect("selected explicit format")
            }
            _ => OutputFormat::resolve(explicit, configured.or(Some("text")))?,
        };
        let timestamp = self.reads.timestamp.clone();
        if let Invocation::Deploy(command) = invocation {
            let executor = deploy_http::DeployHttp(self.http);
            let result = if let Some(files) = self.transfer_files.as_deref_mut() {
                crate::deploy::DeployHost {
                    client: self.reads.sdk,
                    executor: &executor,
                    files,
                    ui: self.transfer_ui,
                    definition_limits: Default::default(),
                    yaml_limits: Default::default(),
                    operation_limits: Default::default(),
                }
                .execute(command, cancelled.as_mut())
                .await
            } else {
                Err(crate::deploy::DeployError {
                    failure: crate::deploy::DeployFailure::Command(errors::unsupported(
                        "Deployment requires an explicit stable local file root",
                    )),
                })
            };
            return Ok(FamilyOutput {
                plan: crate::deploy::render(&result, timestamp, format)?,
                evidence: FamilyEvidence::Deploy(result),
            });
        }
        let mut context = CommandContext::default();
        let current = match context
            .resolve(
                self.reads,
                self.http,
                self.reads.tenant.clone(),
                self.reads.current_path,
                ResolveOptions::default(),
                cancelled.as_mut(),
            )
            .await
        {
            Ok(current) => current,
            Err(error) => return self.command_failure(error, invocation, format),
        };
        match invocation {
            Invocation::Local(command) => {
                if let Err(error) = self.navigation.change(self.store, current.clone(), false) {
                    return self.command_failure(error.command_error(), invocation, format);
                }
                let path = match &command.command {
                    fabric_cli_local::LocalCommand::ChangeDirectory { path }
                    | fabric_cli_local::LocalCommand::Open { path } => Some(path),
                    fabric_cli_local::LocalCommand::Describe { path }
                        if !path.join(" ").starts_with('.') =>
                    {
                        Some(path)
                    }
                    _ => None,
                };
                if let Some(path) = path
                    && let Err(error) = context
                        .resolve(
                            self.reads,
                            self.http,
                            current
                                .fabric()
                                .cloned()
                                .unwrap_or_else(|| self.reads.tenant.clone()),
                            &path.join(" "),
                            ResolveOptions::default(),
                            cancelled.as_mut(),
                        )
                        .await
                {
                    return self.command_failure(error, invocation, format);
                }
                let mut host = NavigationHost {
                    state: self.navigation,
                    lookup: &context,
                    store: self.store,
                    effects: self.local,
                };
                let plan = match crate::local_commands::dispatch(
                    command, self.store, &mut host, timestamp,
                ) {
                    Ok(plan) => plan,
                    Err(fabric_cli_local::LocalError::Text(error)) => return Err(error),
                    Err(error) => {
                        return self.command_failure(error.command_error(), invocation, format);
                    }
                };
                Ok(FamilyOutput {
                    plan,
                    evidence: FamilyEvidence::Local,
                })
            }
            Invocation::Access(command) => {
                let path = match &command.kind {
                    AccessKind::AclList { path, .. }
                    | AccessKind::AclGet { path, .. }
                    | AccessKind::AclChange { path, .. }
                    | AccessKind::LabelChange { path, .. } => Some(path),
                    _ => None,
                };
                if let Some(path) = path
                    && let Err(error) = context
                        .resolve(
                            self.reads,
                            self.http,
                            current
                                .fabric()
                                .cloned()
                                .unwrap_or_else(|| self.reads.tenant.clone()),
                            &path.join(" "),
                            ResolveOptions::default(),
                            cancelled.as_mut(),
                        )
                        .await
                {
                    return self.command_failure(error, invocation, format);
                }
                let api = |audience| {
                    let selection = fabric_sdk::CredentialSelection::new(
                        self.reads.sdk.selection().account(),
                        self.reads.sdk.selection().tenant(),
                        audience,
                    )
                    .expect("validated existing account and tenant");
                    fabric_sdk_api::ApiClient::new(
                        self.reads.sdk.credential().clone(),
                        selection,
                        Arc::new(fabric_cli_resources::ResourceHttpExecutor(Arc::new(
                            self.http.clone(),
                        ))),
                    )
                };
                let fabric = api(fabric_sdk::Audience::Fabric);
                let storage = api(fabric_sdk::Audience::OneLake);
                let arm = api(fabric_sdk::Audience::Arm);
                let resolver = Resolver::new(
                    &context,
                    current
                        .fabric()
                        .cloned()
                        .unwrap_or_else(|| self.reads.tenant.clone()),
                    self.reads.home.clone(),
                );
                let labels = settings
                    .get("local_definition_labels")
                    .and_then(Value::as_str)
                    .map(Path::new);
                let result = AccessHost {
                    clients: ApiClients {
                        fabric: &fabric,
                        onelake: Some(&storage),
                        arm: Some(&arm),
                        powerbi: Some(&fabric),
                    },
                    targets: &resolver,
                    label_definitions: labels,
                    confirmation: self.confirmation.as_mut().map(|confirmation| {
                        &mut **confirmation as &mut dyn fabric_cli_resources::Confirmation
                    }),
                    multipart_boundary: "fabric-cli-native-multipart",
                    limits: Default::default(),
                    request_bounds: Default::default(),
                }
                .execute(command, cancelled.as_mut())
                .await;
                let output = frontdoor::render(
                    command,
                    result.clone(),
                    timestamp,
                    &FamilyPresentation::new(format, command.surface()),
                    &text::ascii_width,
                )?;
                let mut plan = output.plan;
                for warning in output.warnings {
                    plan.records.extend(
                        text::render_notice(
                            text::Notice::Warning(&warning),
                            None,
                            Presentation::default(),
                        )
                        .records,
                    );
                }
                Ok(FamilyOutput {
                    plan,
                    evidence: FamilyEvidence::Access(result),
                })
            }
            Invocation::Resource(command) => {
                let options = ResolveOptions {
                    raise_error: !matches!(
                        command,
                        ResourceCommand::Exists(_)
                            | ResourceCommand::Mkdir { .. }
                            | ResourceCommand::Link { .. }
                    ),
                    supports_local_path: false,
                };
                if let Err(error) = context
                    .resolve(
                        self.reads,
                        self.http,
                        current
                            .fabric()
                            .cloned()
                            .unwrap_or_else(|| self.reads.tenant.clone()),
                        &command.args().path.join(" "),
                        options,
                        cancelled.as_mut(),
                    )
                    .await
                {
                    return self.command_failure(error, invocation, format);
                }
                let other = match command {
                    ResourceCommand::Copy { destination, .. }
                    | ResourceCommand::Move { destination, .. } => Some(destination),
                    ResourceCommand::Assign { workspace, .. }
                    | ResourceCommand::Unassign { workspace, .. } => Some(workspace),
                    _ => None,
                };
                if let Some(path) = other
                    && let Err(error) = context
                        .resolve(
                            self.reads,
                            self.http,
                            current
                                .fabric()
                                .cloned()
                                .unwrap_or_else(|| self.reads.tenant.clone()),
                            &path.join(" "),
                            ResolveOptions {
                                raise_error: false,
                                supports_local_path: false,
                            },
                            cancelled.as_mut(),
                        )
                        .await
                {
                    return self.command_failure(error, invocation, format);
                }
                let resolver =
                    PathResolver::new(&context, current.clone(), self.reads.home.clone(), "ls")
                        .map_err(|_| TextError::InvalidTableEntries)?;
                let mut targets = ResourceTargets { resolver };
                if matches!(
                    command,
                    ResourceCommand::Copy { .. } | ResourceCommand::Move { .. }
                ) {
                    let storage = storage_client(self.reads);
                    let adapter = OneLakeHttpExecutor::new(Arc::new(self.http.clone()));
                    let result = ResourceTransferHost {
                        context: &mut targets,
                        storage: OneLakeWriteHost {
                            client: &storage,
                            executor: &adapter,
                            ui: self.mutations,
                            read_limits: OneLakeLimits::default(),
                            write_limits: OneLakeWriteLimits::default(),
                        },
                        files: self
                            .resource_files
                            .as_mut()
                            .map(|files| &mut **files as &mut dyn ResourceFiles),
                    }
                    .execute(command, cancelled.as_mut())
                    .await;
                    let outputs = match &result {
                        Ok(result) => result.outputs.clone(),
                        Err(error) => vec![error.output()],
                    };
                    let plan = render_outputs(outputs, command.name(), timestamp, format, false)?;
                    return Ok(FamilyOutput {
                        plan,
                        evidence: FamilyEvidence::Storage(result),
                    });
                }
                let mut backend = FabricMutationBackend::new(
                    self.reads.sdk.credential().clone(),
                    self.reads.sdk.selection().clone(),
                    self.http,
                    OperationLimits::default(),
                    cancelled.as_mut(),
                );
                let mut metadata = Metadata { reads: self.reads };
                let result = ResourceHost {
                    context: &mut targets,
                    mutations: MutationHost {
                        operations: &mut backend,
                        ui: self.mutations,
                        metadata: &mut metadata,
                    },
                    config: ResourceConfig {
                        default_capacity_id: settings
                            .get("default_capacity")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                }
                .execute(command)
                .await;
                let (outputs, headers) = match &result {
                    Ok(result) => (result.outputs.clone(), result.show_headers),
                    Err(error) => {
                        let mut outputs = Vec::new();
                        if let Some(creation) = &error.creation
                            && !creation.batch_items.is_empty()
                        {
                            outputs.push(OutputInput {
                                data: Some(Value::Array(creation.batch_items.clone())),
                                ..Default::default()
                            });
                        }
                        outputs.push(error.output());
                        (outputs, false)
                    }
                };
                let plan = render_outputs(outputs, command.name(), timestamp, format, headers)?;
                Ok(FamilyOutput {
                    plan,
                    evidence: FamilyEvidence::Resource(result),
                })
            }
            Invocation::Transfer(command) => {
                let metadata = TransferTargets {
                    reads: self.reads,
                    http: self.http,
                    folders: Mutex::default(),
                    item_sort: if settings
                        .get("output_item_sort_criteria")
                        .and_then(Value::as_str)
                        == Some("bytype")
                    {
                        crate::ItemSort::ByType
                    } else {
                        crate::ItemSort::ByName
                    },
                };
                let result = if let Some(files) = self.transfer_files.as_deref_mut() {
                    let mut export_files;
                    let files: &mut dyn TransferFiles = if matches!(
                        command.kind,
                        TransferKind::Export | TransferKind::BulkExport
                    ) {
                        export_files = export_files::ExportFiles::new(files, self.reads, self.http);
                        &mut export_files
                    } else {
                        files
                    };
                    TransferHost {
                        client: self.reads.sdk,
                        executor: self.http,
                        metadata: &metadata,
                        files,
                        ui: self.transfer_ui,
                        definition_limits: Default::default(),
                        operation_limits: Default::default(),
                        command_line: true,
                    }
                    .execute(command, cancelled.as_mut())
                    .await
                } else {
                    Err(errors::unsupported(
                        "Definition transfer requires an explicit stable local file root",
                    )
                    .into())
                };
                let plan = match &result {
                    Ok(result) => fabric_cli_transfer::render_result(
                        result,
                        command,
                        timestamp,
                        format,
                        Presentation::default(),
                        &text::ascii_width,
                    )?,
                    Err(error) => fabric_cli_transfer::render_error(
                        error,
                        command,
                        timestamp,
                        format,
                        Presentation::default(),
                    )?,
                };
                Ok(FamilyOutput {
                    plan,
                    evidence: FamilyEvidence::Transfer(result),
                })
            }
            _ => self.command_failure(
                errors::unsupported("Expected a mounted family invocation"),
                invocation,
                format,
            ),
        }
    }

    fn command_failure(
        &self,
        error: CommandError,
        invocation: &Invocation,
        format: OutputFormat,
    ) -> Result<FamilyOutput, TextError> {
        let name = match invocation {
            Invocation::Local(command) => command.command.command_path(),
            Invocation::Access(command) => command.command_name(),
            Invocation::Resource(command) => command.name(),
            Invocation::Transfer(command) => command.kind.as_str(),
            Invocation::Deploy(_) => "deploy",
            _ => "command",
        };
        let plan = text::render(
            &error.envelope(self.reads.timestamp.clone(), Some(name.into())),
            format,
            &TextOptions::default(),
            &text::ascii_width,
            Presentation::default(),
        )?;
        Ok(FamilyOutput {
            plan,
            evidence: FamilyEvidence::Local,
        })
    }
}

pub(super) fn storage_client(reads: &ReadSession<'_>) -> OneLakeClient {
    OneLakeClient::new(
        reads.sdk.credential().clone(),
        fabric_sdk::CredentialSelection::new(
            reads.sdk.selection().account(),
            reads.sdk.selection().tenant(),
            fabric_sdk::Audience::OneLake,
        )
        .expect("validated existing account and tenant"),
    )
}

fn render_outputs(
    outputs: Vec<OutputInput>,
    command: &str,
    timestamp: Timestamp,
    format: OutputFormat,
    headers: bool,
) -> Result<TextPlan, TextError> {
    let mut plan = TextPlan {
        records: Vec::new(),
        exit_code: 0,
    };
    for mut output in outputs {
        output.command = Some(command.into());
        let rendered = match text::render(
            &OutputEnvelope::new(timestamp.clone(), output),
            format,
            &TextOptions {
                show_headers: headers,
                ..Default::default()
            },
            &text::ascii_width,
            Presentation::default(),
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                if let Some(partial) = error.partial_output() {
                    plan.records.extend(partial.records.clone());
                }
                return Err(TextError::AfterOutput {
                    error: Box::new(error),
                    partial: Box::new(plan),
                });
            }
        };
        plan.records.extend(rendered.records);
        plan.exit_code = rendered.exit_code;
    }
    Ok(plan)
}

struct ResourceTargets<'a> {
    resolver: PathResolver<'a, CommandContext>,
}

impl ResourceContext for ResourceTargets<'_> {
    fn resolve(
        &mut self,
        tokens: &[String],
        options: ResolveOptions,
    ) -> Result<Arc<PathElement>, MutationError> {
        let tokens = tokens.iter().map(String::as_str).collect::<Vec<_>>();
        let result = self
            .resolver
            .resolve(PathInput::Tokens(&tokens), options)
            .map_err(|error| MutationError::new(&error.error.code, error.error.message))?;
        if !result.notices.is_empty() {
            return Err(MutationError::new(
                "NotSupported",
                "Path notices require an interactive context host",
            ));
        }
        Ok(result.element)
    }
}

struct Metadata<'a, 'session> {
    reads: &'a ReadSession<'session>,
}

impl MutationMetadata for Metadata<'_, '_> {
    fn read<'a>(
        &'a mut self,
        element: &'a Arc<Element>,
    ) -> fabric_cli_mutations::CommandFuture<'a, Result<Value, MutationError>> {
        Box::pin(async move {
            let id = element
                .id()
                .ok_or_else(|| MutationError::new("NotFound", "The resource has no ID"))?;
            match element.kind() {
                NodeKind::Workspace(_) => self
                    .reads
                    .sdk
                    .get_workspace_metadata(self.reads.executor, id, Default::default(), pending())
                    .await
                    .map(|result| result.metadata.into_value())
                    .map_err(|error| {
                        use fabric_sdk::workspaces::WorkspaceMetadataErrorKind;
                        match &error.kind {
                            WorkspaceMetadataErrorKind::Read(kind) => {
                                mutation_error(errors::workspace(kind), &self.reads.timestamp)
                            }
                            WorkspaceMetadataErrorKind::InvalidIdentifier => MutationError::new(
                                "InvalidInput",
                                "Invalid workspace request identifier",
                            ),
                        }
                    }),
                NodeKind::Item(kind) => {
                    let workspace = element
                        .workspace()
                        .and_then(|workspace| workspace.id().map(str::to_owned))
                        .ok_or_else(|| {
                            MutationError::new("InvalidPath", "Item has no workspace")
                        })?;
                    let collection =
                        fabric_sdk::items::ItemCollection::from_item_type(kind.as_str())
                            .ok_or_else(|| {
                                MutationError::new(
                                    "NotSupported",
                                    "Item metadata route is not mounted",
                                )
                            })?;
                    self.reads
                        .sdk
                        .get_item_metadata_from_collection(
                            self.reads.executor,
                            &workspace,
                            collection,
                            id,
                            Default::default(),
                            pending(),
                        )
                        .await
                        .map(|metadata| metadata.into_value())
                        .map_err(|error| {
                            let error = errors::item(&error.kind);
                            mutation_error(error, &self.reads.timestamp)
                        })
                }

                _ => Err(MutationError::new(
                    "NotSupported",
                    "Metadata update for this path is not mounted",
                )),
            }
        })
    }
}

fn mutation_error(error: CommandError, timestamp: &Timestamp) -> MutationError {
    let envelope = error.envelope(timestamp.clone(), None);
    MutationError::new(
        envelope.result().error_code().unwrap_or("UnknownError"),
        envelope
            .result()
            .message()
            .unwrap_or("Metadata read failed"),
    )
}

struct TransferTargets<'a, 'session> {
    reads: &'a ReadSession<'session>,
    http: &'a HttpExecutor,
    item_sort: crate::ItemSort,
    folders: Mutex<std::collections::BTreeMap<String, Vec<Arc<Element>>>>,
}

impl TransferMetadata for TransferTargets<'_, '_> {
    fn resolve<'a>(
        &'a self,
        kind: TransferKind,
        path: &'a [String],
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<TransferContext, CommandError>> {
        Box::pin(async move {
            let context = self.resolve_target(kind, path).await?;
            self.complete_snapshot(kind, context).await
        })
    }

    fn resolve_target<'a>(
        &'a self,
        kind: TransferKind,
        path: &'a [String],
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<TransferContext, CommandError>> {
        Box::pin(async move {
            let mut context = CommandContext::default();
            let current = context
                .resolve(
                    self.reads,
                    self.http,
                    self.reads.tenant.clone(),
                    self.reads.current_path,
                    ResolveOptions::default(),
                    pending(),
                )
                .await?;
            let target = context
                .resolve_path(
                    self.reads,
                    self.http,
                    current,
                    PathInput::Scalar(&path.join(" ")),
                    ResolveOptions {
                        raise_error: kind != TransferKind::Import,
                        supports_local_path: false,
                    },
                    pending(),
                )
                .await?;
            let target = target.fabric().cloned().ok_or_else(|| {
                errors::unsupported("Definition transfer requires a Fabric item or workspace")
            })?;
            let support =
                fabric_cli_local::SupportMatrix::source().map_err(|error| error.command_error())?;
            if matches!(target.kind(), NodeKind::Item(_)) && kind != TransferKind::BulkExport {
                support
                    .check(&PathElement::from_fabric(target.clone()), kind.as_str())
                    .map_err(|error| error.command_error())?;
            }
            if let Some(workspace) = target.workspace()
                && let Some(folders) = context.cached_folders(workspace.clone())?
            {
                self.folders
                    .lock()
                    .map_err(|_| {
                        errors::error("MetadataUnavailable", "Transfer snapshot is unavailable")
                    })?
                    .insert(workspace.id().unwrap_or_default().into(), folders);
            }
            Ok(TransferContext {
                target,
                items: Vec::new(),
                selection_order: Vec::new(),
            })
        })
    }

    fn complete_snapshot<'a>(
        &'a self,
        kind: TransferKind,
        context: TransferContext,
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<TransferContext, CommandError>> {
        Box::pin(async move {
            let target = context.target;
            let support =
                fabric_cli_local::SupportMatrix::source().map_err(|error| error.command_error())?;
            let mut items = Vec::new();
            if matches!(target.kind(), NodeKind::Workspace(_) | NodeKind::Folder) {
                let workspace = target.workspace().ok_or_else(|| {
                    errors::error("InvalidInput", "Workspace metadata is required")
                })?;
                let list = self
                    .reads
                    .sdk
                    .list_items(
                        self.reads.executor,
                        workspace
                            .id()
                            .ok_or_else(|| errors::error("NotFound", "Workspace has no ID"))?,
                        Default::default(),
                        pending(),
                    )
                    .await
                    .map_err(|error| errors::item(&error.kind))?;
                let cached = self
                    .folders
                    .lock()
                    .map_err(|_| {
                        errors::error("MetadataUnavailable", "Transfer snapshot is unavailable")
                    })?
                    .get(workspace.id().unwrap_or_default())
                    .cloned();
                let folders = if let Some(folders) = cached {
                    folders
                } else if target.kind() == NodeKind::Folder
                    || list
                        .value
                        .iter()
                        .any(|item| item.additional_fields.contains_key("folderId"))
                {
                    let folders = self
                        .reads
                        .sdk
                        .list_folders(
                            self.reads.executor,
                            workspace
                                .id()
                                .ok_or_else(|| errors::error("NotFound", "Workspace has no ID"))?,
                            Default::default(),
                            pending(),
                        )
                        .await
                        .map_err(|error| crate::folder_reads::folder_error(&error.kind))?;
                    crate::folder_reads::FolderGraph::new(folders.value)
                        .map_err(|error| error.command_error())?
                        .elements(workspace.clone())
                        .map_err(|error| errors::error(&error.code, error.message))?
                } else {
                    Vec::new()
                };
                for item in list.value {
                    let parent = match item.additional_fields.get("folderId") {
                        None => workspace.clone(),
                        Some(id) => folders
                            .iter()
                            .find(|folder| folder.id() == id.as_str())
                            .filter(|_| id.is_string())
                            .cloned()
                            .ok_or_else(|| {
                                errors::error(
                                    "InvalidResponse",
                                    "Item folder metadata is missing or invalid",
                                )
                            })?,
                    };
                    if !item.additional_fields.contains_key("type") {
                        return Err(errors::error(
                            "InvalidResponse",
                            "Fabric returned an item without its type",
                        ));
                    }
                    let Some(item_type) = item
                        .item_type()
                        .and_then(|kind| fabric_cli_context::ItemType::parse(kind).ok())
                    else {
                        continue;
                    };
                    let element = Element::child(
                        &item.display_name,
                        Some(item.id),
                        NodeKind::Item(item_type),
                        parent,
                    )
                    .map_err(|error| errors::error(&error.code, error.message))?;
                    let policy = if kind == TransferKind::BulkExport {
                        "bulkexport"
                    } else {
                        kind.as_str()
                    };
                    let supported =
                        match support.check(&PathElement::from_fabric(element.clone()), policy) {
                            Ok(()) => fabric_deploy::definitions::CommandSupport::Supported,
                            Err(fabric_cli_local::LocalError::Context(error))
                                if error.code == "UnsupportedCommand" =>
                            {
                                fabric_deploy::definitions::CommandSupport::Rejected
                            }
                            Err(error) => return Err(error.command_error()),
                        };
                    items.push(fabric_cli_transfer::TransferItem {
                        element,
                        support: supported,
                    });
                }
                if target.kind() == NodeKind::Folder {
                    items = folder_transfer_order(&target, &folders, &items)?;
                }
            }
            let mut selection_items = items
                .iter()
                .filter(|item| {
                    item.support == fabric_deploy::definitions::CommandSupport::Supported
                })
                .collect::<Vec<_>>();
            selection_items.sort_by_cached_key(|item| {
                let NodeKind::Item(kind) = item.element.kind() else {
                    unreachable!()
                };
                let name = item.element.short_name().to_lowercase();
                let kind = crate::reads::enum_name(kind).to_owned();
                match self.item_sort {
                    crate::ItemSort::ByName => (name, kind),
                    crate::ItemSort::ByType => (kind, name),
                }
            });
            let selection_order = selection_items
                .iter()
                .map(|item| item.element.name())
                .collect();
            Ok(TransferContext {
                target,
                items,
                selection_order,
            })
        })
    }

    fn folder_transfer_order(
        target: &Arc<Element>,
        folders: &[Arc<Element>],
        items: &[fabric_cli_transfer::TransferItem],
    ) -> Result<Vec<fabric_cli_transfer::TransferItem>, CommandError> {
        let mut result = Vec::new();
        let mut stack = vec![(target.clone(), false, 0usize)];
        while let Some((folder, visited, depth)) = stack.pop() {
            if depth
                > fabric_deploy::definitions::DefinitionLimits::default()
                    .max_depth
                    .get()
            {
                return Err(errors::error(
                    "Limit",
                    "Folder transfer depth bound reached",
                ));
            }
            if visited {
                result.extend(
                    items
                        .iter()
                        .filter(|item| item.element.parent() == Some(&folder))
                        .cloned(),
                );
            } else {
                // Source visits child folders before the direct items at each level.
                stack.push((folder.clone(), true, depth));
                stack.extend(
                    folders
                        .iter()
                        .rev()
                        .filter(|child| child.parent() == Some(&folder))
                        .map(|child| (child.clone(), false, depth + 1)),
                );
            }
        }
        Ok(result)
    }
}
