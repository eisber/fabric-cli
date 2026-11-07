// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, opt-in Ontology root composition, separate from the v1 grammar.
//!
//! Both entry points call the released capability adapter with a caller-owned
//! registry, policy and I/O. Owner discovery reuses the ordinary read session.
//! No runtime, credential selection, private workload binding or default approval
//! is installed here. The standalone executable remains explicitly unconfigured.

use crate::session::{ReadSession, snapshot::Snapshot};
use clap::{Command, error::ErrorKind};
use fabric_cli_context::NodeKind;
use fabric_cli_ontology::{
    AdapterError, AdapterHost, AdapterLimits, CapabilityError, CapabilityReport, ExecutionReport,
    HostError, InputSource, NativeContext, NativeHost, OntologyArguments, OntologyTarget,
    OntologyVerb, OutputDestination, execute, execute_metadata, execute_native, ontology_command,
    operation_command, parse_common_args, parse_facade_args, validate_arguments,
};
use fabric_cli_output::CommandError;
use fabric_sdk::{
    Audience, RequestContext,
    items::{ItemClient, ItemExecutor, ItemGetOptions},
    ontology::{self, ObservedGeneration, PublicError},
    v2::{
        ApprovalPolicy, ChildIdentity, ExecutionReceipt, ItemIdentity, PreparedOperation, Registry,
        ResolvedTarget, Target, TargetKind, WorkspaceIdentity,
    },
};
use fabric_sdk_operations::OperationExecutor;
use std::{
    ffi::OsString,
    fmt,
    future::Future,
    io::{self, Write},
    num::NonZeroUsize,
    pin::pin,
    time::Duration,
};

/// One root request; the facade changes only the owner restriction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewCommand {
    arguments: OntologyArguments,
    facade: bool,
}

impl PreviewCommand {
    /// Returns the shared capability arguments without rewriting their semantics.
    pub fn arguments(&self) -> &OntologyArguments {
        &self.arguments
    }

    /// Whether `artifact ontology` supplied the explicit owner restriction.
    pub fn is_facade(&self) -> bool {
        self.facade
    }
}

/// Help/version is handled before host or configuration access.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewInvocation {
    /// Human-readable schema output.
    Display(String),
    /// A locally parsed request, not approval or successful execution.
    Operation(Box<PreviewCommand>),
}

/// Root failures retain original SDK/adapter evidence without displaying secrets.
#[derive(Debug)]
pub enum PreviewError {
    /// Invalid syntax or unsupported local option combination.
    Arguments,
    /// No approved registry/policy/I/O binding is installed in this executable.
    NotConfigured,
    /// A facet or verb outside this bounded public execution slice.
    Unsupported,
    /// The caller's tenant or audience does not match the read session.
    Context,
    /// Original existing owner-resolver error.
    Discovery(CommandError),
    /// Original public metadata failure while establishing type/generation.
    Metadata(Box<PublicError>),
    /// Caller refused discovery or deadline binding.
    Host(HostError),
    /// Original capability failure, including post-dispatch output evidence.
    Adapter(AdapterError),
    /// Original metadata mutation failure, including post-dispatch output evidence.
    MetadataMutation(CapabilityError),
    /// Original private workload failure, including post-dispatch uncertainty.
    Native(CapabilityError),
}

impl PreviewError {
    /// Stable nonzero exit; accepted operations are classified by their report.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Arguments
            | Self::Context
            | Self::Adapter(AdapterError::Invalid(_))
            | Self::MetadataMutation(
                CapabilityError::Invalid(_) | CapabilityError::Validation(_),
            )
            | Self::Native(CapabilityError::Invalid(_) | CapabilityError::Validation(_)) => 2,
            _ => 1,
        }
    }

    /// Writes only a sanitized local diagnostic, never raw provider/parser text.
    ///
    /// Adapter execution already writes failure receipts. Do not call this again
    /// for an executed report or output error; retain its original evidence.
    ///
    /// # Errors
    /// Returns the original diagnostic writer failure.
    pub fn write_diagnostic(&self, stderr: &mut impl Write) -> io::Result<()> {
        let (code, message) = match self {
            Self::Arguments => (
                "invalid_arguments",
                "Invalid preview arguments. Run fab-rs --v2-preview --help.",
            ),
            Self::NotConfigured => (
                "NotConfigured",
                "Ontology preview execution requires an injected registry, verified public binding, discovery policy, exact-effect approval, finite deadline and I/O host. Native credentials alone do not configure this binding.",
            ),
            Self::Unsupported => (
                "NotSupported",
                "This preview supports bounded Ontology metadata/source operations and separately injected native entity operations only.",
            ),
            Self::Context => (
                "invalid_configuration",
                "The preview read session requires a matching Fabric tenant and audience.",
            ),
            Self::Discovery(_) => (
                "ResolutionFailed",
                "Ontology owner resolution failed; original typed evidence is retained.",
            ),
            Self::Metadata(_) => (
                "MetadataFailed",
                "Ontology type/generation discovery failed; no source fallback was attempted.",
            ),
            Self::Host(_) => (
                "HostRefused",
                "The host refused Ontology preview preflight.",
            ),
            Self::Adapter(_) => (
                "OntologyFailed",
                "Ontology preview failed; retain the original adapter evidence and do not replay uncertain work.",
            ),
            Self::MetadataMutation(_) | Self::Native(_) => (
                "OntologyFailed",
                "Ontology metadata mutation failed; retain the original SDK evidence and do not replay uncertain work.",
            ),
        };
        serde_json::to_writer(
            &mut *stderr,
            &serde_json::json!({"error":{"code":code,"message":message}}),
        )
        .map_err(io::Error::other)?;
        writeln!(stderr)
    }
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ontology preview failed; inspect the retained typed cause")
    }
}
impl std::error::Error for PreviewError {}

/// Maps an actual adapter report to a root exit without claiming completion.
///
/// Accepted submission returns zero but retains its nonterminal receipt. Partial,
/// failed and unknown outcomes return one; output errors remain typed errors.
pub fn execution_exit_code(report: &ExecutionReport) -> u8 {
    u8::from(!report.command_succeeded())
}

/// Original public or metadata mutation report, without projecting away failures.
#[derive(Debug)]
pub enum PreviewReport {
    /// Public metadata Get or definition/model Get/replacement report.
    Public(ExecutionReport),
    /// Metadata-only item Create/Patch/Delete report.
    Metadata(CapabilityReport),
    /// Private native entity report, never a public endpoint fallback.
    Native(CapabilityReport),
}

impl PreviewReport {
    /// Returns the original neutral receipt for either execution path.
    pub fn receipt(&self) -> &ExecutionReceipt<serde_json::Value> {
        match self {
            Self::Public(report) => &report.receipt,
            Self::Metadata(report) | Self::Native(report) => &report.receipt,
        }
    }

    /// Accepted submission succeeds as submission, never as completion.
    pub fn command_succeeded(&self) -> bool {
        match self {
            Self::Public(report) => report.command_succeeded(),
            Self::Metadata(report) | Self::Native(report) => report.command_succeeded(),
        }
    }

    /// Root exit for an actual report; partial/failed/unknown remain nonzero.
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.command_succeeded())
    }
}

/// Builds an isolated root schema, not a replacement for the existing parser.
pub fn command() -> Command {
    let ontology = ontology_command()
        .disable_help_flag(false)
        .mut_subcommands(|operation| operation.disable_help_flag(false));
    OntologyVerb::ALL.into_iter().fold(
        Command::new("fab-rs-v2-preview")
            .bin_name("fab-rs --v2-preview")
            .version(env!("CARGO_PKG_VERSION"))
            .about("Opt-in injected public Ontology execution preview")
            .subcommand_required(true)
            .disable_help_subcommand(true)
            .subcommand(
                Command::new("artifact")
                    .subcommand_required(true)
                    .disable_help_subcommand(true)
                    .subcommand(ontology),
            ),
        |root, verb| root.subcommand(operation_command(verb).disable_help_flag(false)),
    )
}

/// Parses argv including its program name, after the outer preview marker.
///
/// Generic actions and `artifact ontology` reuse the same capability schema.
/// No paths are resolved, files opened or credentials accessed.
///
/// # Errors
/// Rejects invalid syntax without echoing supplied values.
pub fn parse_args<I, T>(args: I) -> Result<PreviewInvocation, PreviewError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let matches = match command().try_get_matches_from(&args) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(PreviewInvocation::Display(error.to_string()));
        }
        Err(_) => return Err(PreviewError::Arguments),
    };
    let (name, _) = matches.subcommand().ok_or(PreviewError::Arguments)?;
    let facade = name == "artifact";
    let arguments = if facade {
        parse_facade_args(args.iter().skip(2).cloned())
    } else {
        let verb = OntologyVerb::ALL
            .into_iter()
            .find(|verb| verb.common_name() == name)
            .ok_or(PreviewError::Arguments)?;
        parse_common_args(verb, args.iter().skip(1).cloned())
    }
    .map_err(|_| PreviewError::Arguments)?;
    Ok(PreviewInvocation::Operation(Box::new(PreviewCommand {
        arguments,
        facade,
    })))
}

/// Standalone root entry without a verified caller binding.
///
/// This deliberately does not enter native setup, load settings or acquire a
/// credential. A native-read build does not prove Ontology's conditional gates.
///
/// # Errors
/// Returns output failures without claiming that a provider operation failed.
pub fn run_unconfigured<I, T>(
    args: I,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let error = match parse_args(args) {
        Ok(PreviewInvocation::Display(text)) => {
            stdout.write_all(text.as_bytes())?;
            return Ok(0);
        }
        Ok(PreviewInvocation::Operation(_)) => PreviewError::NotConfigured,
        Err(error) => error,
    };
    error.write_diagnostic(stderr)?;
    Ok(error.exit_code())
}

/// Caller-owned root I/O and policy; no blanket approval implementation exists.
///
/// `authorize_discovery` must approve bounded workspace/item metadata reads,
/// including public Ontology metadata, for this exact path and SDK context.
/// `authorize` then verifies all conditional gates and the exact prepared
/// operation, including metering and unguarded replacement concurrency.
pub trait PreviewHost: ApprovalPolicy<Error = HostError> {
    /// Bind one finite total deadline to the supplied cancellation future.
    ///
    /// This is called once before discovery, not restarted before dispatch.
    fn deadline(&mut self, explicit: Option<Duration>) -> Result<(), HostError>;
    /// Approve discovery only, not source retrieval or mutation.
    fn authorize_discovery(
        &mut self,
        command: &PreviewCommand,
        context: &RequestContext,
    ) -> Result<(), HostError>;
    /// Read bounded explicit input, consuming stdin at most once.
    fn read_input(
        &mut self,
        source: &InputSource,
        maximum: NonZeroUsize,
    ) -> Result<Vec<u8>, HostError>;
    /// Atomically reserve a no-clobber file or supply the selected stdout writer.
    fn reserve_output(
        &mut self,
        destination: &OutputDestination,
    ) -> Result<Box<dyn Write>, HostError>;
    /// Write diagnostics on stderr only.
    fn diagnostic(&mut self, bytes: &[u8]) -> io::Result<()>;
}

/// Additional caller-owned private binding; public credentials never imply it.
pub trait NativePreviewHost: PreviewHost {
    /// Return a verified context without credentials, transport or remote work.
    ///
    /// The caller must bind the native executor to the approved account, tenant,
    /// routing and audience, independently of public metadata discovery.
    fn native_context(&mut self) -> Result<NativeContext, HostError>;
}

struct SelectedPath {
    owner: String,
    workspace: String,
    item_name: String,
    suffix: String,
    facet: &'static str,
    representation: &'static str,
    target: OntologyTarget,
    child: Option<ChildIdentity>,
}

impl SelectedPath {
    fn parse(path: &str) -> Result<Self, PreviewError> {
        if path.len() > 32 * 1024 || path.contains('\\') || path.chars().any(char::is_control) {
            return Err(PreviewError::Arguments);
        }
        let parts: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
        if parts.len() < 2
            || parts
                .iter()
                .any(|part| part.is_empty() || matches!(*part, "." | ".."))
        {
            return Err(PreviewError::Arguments);
        }
        if !parts[0]
            .strip_suffix(".Workspace")
            .is_some_and(|name| !name.trim().is_empty())
            || !parts[1]
                .strip_suffix(".Ontology")
                .is_some_and(|name| !name.trim().is_empty())
        {
            return Err(PreviewError::Unsupported);
        }
        let mut child = None;
        let (suffix, facet, representation, target) = match &parts[2..] {
            [] => ("", "metadata", "json", OntologyTarget::Item),
            ["src", "definition"] => (
                "/src/definition",
                "definition",
                "native",
                OntologyTarget::Definition,
            ),
            ["src", "model"] => (
                "/src/model",
                "model",
                "expanded",
                OntologyTarget::PublicModelExpanded,
            ),
            ["src", "model", "expanded"] => (
                "/src/model/expanded",
                "model",
                "expanded",
                OntologyTarget::PublicModelExpanded,
            ),
            ["src", "model", "minimized"] => (
                "/src/model/minimized",
                "model",
                "minimized",
                OntologyTarget::PublicModelMinimized,
            ),
            ["src", "schema", "entities"] => (
                "/src/schema/entities",
                "schema.entities",
                "native",
                OntologyTarget::Entities,
            ),
            ["src", "schema", "entities", lineage] => {
                child = Some(ChildIdentity {
                    kind: "entity.lineage".into(),
                    id: (*lineage).into(),
                });
                ("", "schema.entity", "native", OntologyTarget::Entity)
            }
            ["src", "schema", "entities", lineage, "binding"] => {
                child = Some(ChildIdentity {
                    kind: "entity.lineage".into(),
                    id: (*lineage).into(),
                });
                (
                    "",
                    "schema.binding",
                    "native",
                    OntologyTarget::EntitySourceBinding,
                )
            }
            _ => return Err(PreviewError::Unsupported),
        };
        Ok(Self {
            owner: format!("/{}/{}", parts[0], parts[1]),
            workspace: format!("/{}", parts[0]),
            item_name: parts[1].into(),
            suffix: if child.is_some() {
                format!("/{}", parts[2..].join("/"))
            } else {
                suffix.into()
            },
            facet,
            representation,
            target,
            child,
        })
    }
}

struct ResolvedHost<'a, H> {
    host: &'a mut H,
    target: ResolvedTarget,
    requested_path: &'a str,
    timeout: Option<Duration>,
    creation_name: Option<&'a str>,
}

impl<H: PreviewHost> ApprovalPolicy for ResolvedHost<'_, H> {
    type Error = HostError;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), HostError> {
        if let Some(name) = self.creation_name
            && operation.input()["request"]["body"]["displayName"].as_str() != Some(name)
        {
            return Err(HostError(
                "Creation displayName must match the addressed item name.".into(),
            ));
        }
        self.host.authorize(operation)
    }
}
impl<H: PreviewHost> AdapterHost for ResolvedHost<'_, H> {
    fn resolve(&mut self, path: &str) -> Result<ResolvedTarget, HostError> {
        if path != self.requested_path {
            return Err(HostError("Resolved target substitution refused.".into()));
        }
        Ok(self.target.clone())
    }
    fn read_input(
        &mut self,
        source: &InputSource,
        maximum: NonZeroUsize,
    ) -> Result<Vec<u8>, HostError> {
        self.host.read_input(source, maximum)
    }
    fn reserve_output(
        &mut self,
        destination: &OutputDestination,
    ) -> Result<Box<dyn Write>, HostError> {
        self.host.reserve_output(destination)
    }
    fn diagnostic(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.host.diagnostic(bytes)
    }
    fn deadline(&mut self, explicit: Option<Duration>) -> Result<(), HostError> {
        if explicit != self.timeout {
            return Err(HostError("Deadline substitution refused.".into()));
        }
        Ok(())
    }
}

impl<H: NativePreviewHost> NativeHost for ResolvedHost<'_, H> {
    fn native_context(&mut self) -> Result<NativeContext, HostError> {
        self.host.native_context()
    }
}

fn begin<H: PreviewHost>(
    command: &PreviewCommand,
    reads: &ReadSession<'_>,
    host: &mut H,
) -> Result<(), PreviewError> {
    if command.arguments.profile.is_some() || command.arguments.project.is_some() {
        return Err(PreviewError::Arguments);
    }
    if reads.tenant.kind() != NodeKind::Tenant
        || reads.tenant.id() != Some(reads.sdk.selection().tenant())
        || reads.sdk.selection().audience() != Audience::Fabric
    {
        return Err(PreviewError::Context);
    }
    host.deadline(command.arguments.timeout)
        .map_err(PreviewError::Host)?;
    host.authorize_discovery(command, reads.sdk.context())
        .map_err(PreviewError::Host)
}

async fn existing_target(
    args: &OntologyArguments,
    path: &SelectedPath,
    reads: &ReadSession<'_>,
    executor: &dyn ItemExecutor,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ResolvedTarget, PreviewError> {
    let mut cancelled = pin!(cancelled);
    let owner = Snapshot::default()
        .resolve(reads, &reads.tenant, &path.owner, cancelled.as_mut())
        .await
        .map_err(PreviewError::Discovery)?;
    if !matches!(owner.kind(), NodeKind::Item(kind) if kind.as_str() == "Ontology") {
        return Err(PreviewError::Unsupported);
    }
    let workspace = owner.workspace().ok_or(PreviewError::Context)?;
    let identity = ItemIdentity::new(
        workspace.id().ok_or(PreviewError::Context)?,
        owner.id().ok_or(PreviewError::Context)?,
    )
    .map_err(|_| PreviewError::Context)?;
    let client = ItemClient::with_context(reads.sdk.context().clone());
    let sdk_target = ontology::OntologyTarget::new(identity.workspace_id(), identity.item_id())
        .map_err(|_| PreviewError::Context)?;
    let metadata = ontology::get_metadata(
        &client,
        executor,
        &sdk_target,
        ItemGetOptions {
            query: vec![],
            max_response_bytes: limits.operation.max_response_bytes,
        },
        cancelled.as_mut(),
    )
    .await
    .map_err(|error| PreviewError::Metadata(Box::new(error)))?;
    let observed = match metadata.generation() {
        ObservedGeneration::Known(value) => Some(value),
        ObservedGeneration::Unknown => None,
    };
    validate_arguments(args, path.target, "Ontology", observed)
        .map_err(|_| PreviewError::Unsupported)?;
    ResolvedTarget::new(
        &format!("{}{}", owner.path(), path.suffix),
        identity,
        Target {
            kind: TargetKind::Item,
            owner_type: "Ontology".into(),
            facet: path.facet.into(),
            representation: path.representation.into(),
            generation: observed.and_then(|value| u32::try_from(value).ok()),
        },
        path.child.clone(),
    )
    .map_err(|_| PreviewError::Context)
}

/// Dispatch both preview entry points through the same public operation routes.
///
/// Existing [`dispatch`] callers retain the original public report API.
/// Metadata Create/Patch/Delete use [`dispatch_metadata`] and retain their
/// original report/cause. Private native execution requires [`dispatch_with_native`].
///
/// # Errors
/// Retains the selected adapter's typed error and any original output evidence.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit borrowed root host capabilities."
)]
pub async fn dispatch_command<E, H>(
    command: &PreviewCommand,
    reads: &ReadSession<'_>,
    executor: E,
    registry: &Registry,
    host: &mut H,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<PreviewReport, PreviewError>
where
    E: OperationExecutor + ItemExecutor,
    H: PreviewHost,
{
    let path = SelectedPath::parse(&command.arguments.path)?;
    if path.target == OntologyTarget::Item
        && matches!(
            command.arguments.verb,
            OntologyVerb::Create | OntologyVerb::Update | OntologyVerb::Delete
        )
    {
        dispatch_metadata(command, reads, executor, registry, host, limits, cancelled)
            .await
            .map(PreviewReport::Metadata)
    } else {
        dispatch(command, reads, executor, registry, host, limits, cancelled)
            .await
            .map(PreviewReport::Public)
    }
}

/// Dispatch public and explicitly bound private native entity operations.
///
/// The same generic/facade arguments, real owner discovery, immutable approval
/// and output policy apply. Private schema members are single opaque lineage
/// segments, not display names or generation-1 numeric selectors. This mount
/// neither installs private routing/authentication nor parses encoded IDs.
///
/// # Errors
/// Retains the selected adapter error and any original dispatch/output evidence.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit borrowed root host capabilities."
)]
pub async fn dispatch_with_native<E, H>(
    command: &PreviewCommand,
    reads: &ReadSession<'_>,
    executor: E,
    registry: &Registry,
    host: &mut H,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<PreviewReport, PreviewError>
where
    E: OperationExecutor + ItemExecutor,
    H: NativePreviewHost,
{
    let path = SelectedPath::parse(&command.arguments.path)?;
    if !matches!(
        path.target,
        OntologyTarget::Entities | OntologyTarget::Entity | OntologyTarget::EntitySourceBinding
    ) {
        return dispatch_command(command, reads, executor, registry, host, limits, cancelled).await;
    }
    begin(command, reads, host)?;
    let mut cancelled = pin!(cancelled);
    let target = existing_target(
        &command.arguments,
        &path,
        reads,
        &executor,
        limits,
        cancelled.as_mut(),
    )
    .await?;
    let mut resolved = ResolvedHost {
        host,
        target,
        requested_path: &command.arguments.path,
        timeout: command.arguments.timeout,
        creation_name: None,
    };
    execute_native(
        &command.arguments,
        command.facade,
        registry,
        &mut resolved,
        limits,
        cancelled.as_mut(),
    )
    .await
    .map(PreviewReport::Native)
    .map_err(PreviewError::Native)
}

/// Execute metadata-only creation, patch or deletion through the root resolver.
///
/// Create resolves the real workspace only and carries no invented item ID or
/// generation. The root binds displayName to the addressed item name at approval.
/// Fresh-resource creation is not atomic remote display-name reservation; caller
/// policy must refuse any stronger unavailable namespace guard requirement.
/// Patch/Delete require authoritative existing Ontology metadata first.
///
/// # Errors
/// Returns original metadata adapter/SDK/output errors without replay.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit borrowed root host capabilities."
)]
pub async fn dispatch_metadata<E, H>(
    command: &PreviewCommand,
    reads: &ReadSession<'_>,
    executor: E,
    registry: &Registry,
    host: &mut H,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CapabilityReport, PreviewError>
where
    E: OperationExecutor + ItemExecutor,
    H: PreviewHost,
{
    let args = &command.arguments;
    let path = SelectedPath::parse(&args.path)?;
    if path.target != OntologyTarget::Item
        || !matches!(
            args.verb,
            OntologyVerb::Create | OntologyVerb::Update | OntologyVerb::Delete
        )
    {
        return Err(PreviewError::Unsupported);
    }
    begin(command, reads, host)?;
    let mut cancelled = pin!(cancelled);
    let target = if args.verb == OntologyVerb::Create {
        validate_arguments(args, OntologyTarget::Item, "Ontology", None)
            .map_err(|_| PreviewError::Arguments)?;
        let workspace = Snapshot::default()
            .resolve(reads, &reads.tenant, &path.workspace, cancelled.as_mut())
            .await
            .map_err(PreviewError::Discovery)?;
        if !matches!(workspace.kind(), NodeKind::Workspace(_)) {
            return Err(PreviewError::Context);
        }
        ResolvedTarget::workspace_collection(
            &format!("{}/{}", workspace.path(), path.item_name),
            WorkspaceIdentity::new(workspace.id().ok_or(PreviewError::Context)?)
                .map_err(|_| PreviewError::Context)?,
            Target {
                kind: TargetKind::WorkspaceCollection,
                owner_type: "Ontology".into(),
                facet: "metadata".into(),
                representation: "json".into(),
                generation: None,
            },
        )
        .map_err(|_| PreviewError::Context)?
    } else {
        existing_target(args, &path, reads, &executor, limits, cancelled.as_mut()).await?
    };
    let client = ItemClient::with_context(reads.sdk.context().clone());
    let mut resolved = ResolvedHost {
        host,
        target,
        requested_path: &args.path,
        timeout: args.timeout,
        creation_name: if args.verb == OntologyVerb::Create {
            path.item_name.strip_suffix(".Ontology")
        } else {
            None
        },
    };
    execute_metadata(
        args,
        command.facade,
        registry,
        &mut resolved,
        &client,
        executor,
        limits,
        cancelled.as_mut(),
    )
    .await
    .map_err(PreviewError::MetadataMutation)
}

/// Execute a root request using the existing read session and real SDK adapter.
///
/// The supplied registry must already contain the canonical descriptors and
/// `bind_public_operations` bindings. This mount neither invents schemas nor
/// registers another generic head. Supply the same executor/endpoint as `reads`
/// and a cancellation future observing the host's finite total deadline.
///
/// All facets are stripped before the existing owner resolver runs. They can
/// never enter OneLake folder resolution. Unknown generations fail closed in
/// the released adapter; metadata is not substituted for denied source.
///
/// # Errors
/// Retains discovery, public metadata, adapter and output failures. Adapter
/// receipts already select stdout/stderr. Local errors are returned for the
/// caller to render with `write_diagnostic`; no uncertain request is replayed.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit borrowed root host capabilities."
)]
pub async fn dispatch<E, H>(
    command: &PreviewCommand,
    reads: &ReadSession<'_>,
    executor: E,
    registry: &Registry,
    host: &mut H,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ExecutionReport, PreviewError>
where
    E: OperationExecutor + ItemExecutor,
    H: PreviewHost,
{
    let args = &command.arguments;
    let path = SelectedPath::parse(&args.path)?;
    if !matches!(args.verb, OntologyVerb::Get | OntologyVerb::Update)
        || matches!(
            path.target,
            OntologyTarget::Entities | OntologyTarget::Entity | OntologyTarget::EntitySourceBinding
        )
    {
        return Err(PreviewError::Unsupported);
    }
    begin(command, reads, host)?;
    let mut cancelled = pin!(cancelled);
    let target = existing_target(args, &path, reads, &executor, limits, cancelled.as_mut()).await?;
    let client = ItemClient::with_context(reads.sdk.context().clone());
    let mut resolved = ResolvedHost {
        host,
        target,
        requested_path: &args.path,
        timeout: args.timeout,
        creation_name: None,
    };
    execute(
        args,
        command.facade,
        registry,
        &mut resolved,
        &client,
        executor,
        limits,
        cancelled.as_mut(),
    )
    .await
    .map_err(PreviewError::Adapter)
}
