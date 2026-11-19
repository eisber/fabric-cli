// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Collection-only Livy leaf over the existing parser, SDK and shared native host.

use super::{
    OwnerKind, ParsedInvocation, ReadError, ReadErrorKind, ReadFacet, ReadOperation, ReadOptions,
    ResolvedTarget, TypedPath, command_schema,
    host::{self, Destination, ExplicitResolver, HostError, PendingRead},
    parse_args,
};
use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_sdk::{
    EndpointBinding, RequestContext, ServiceOrigin,
    livy::{LivyService, ServiceProfile},
    runtime::NativeEndpoint,
    v2::ItemIdentity,
};
use fabric_sdk_http::{ExecutionTrace, HttpExecutor, native_runtime::NativeFabricExecutor};
use std::{ffi::OsString, future::Future, time::Instant};

/// Explicit Fabric API version qualified by the selected Livy source contract.
pub const FABRIC_API_VERSION: &str = "2023-12-01";

/// One validated classic Livy collection read, never a Notebook or individual read.
pub struct PreparedList {
    invocation: ParsedInvocation,
    options: ReadOptions,
}

impl PreparedList {
    /// Parse the existing runtime grammar, including the program name.
    ///
    /// # Errors
    /// Rejects foreign operations/facets and selections exceeding host ceilings
    /// before binding, credentials, output preflight or HTTP.
    pub fn parse<I, T>(args: I, options: ReadOptions) -> Result<Self, ReadError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let invocation = parse_args(args)?;
        options.validate()?;
        invocation.validate(options)?;
        if !owns(&invocation) {
            return Err(ReadError::new(ReadErrorKind::UnsupportedCapability));
        }
        Ok(Self {
            invocation,
            options,
        })
    }

    /// The unchanged normalized input consumed by the shared runtime host.
    pub fn invocation(&self) -> &ParsedInvocation {
        &self.invocation
    }

    /// Bind the exact selected human owner to an explicitly supplied SDK profile.
    ///
    /// This does not authorize an executor for the service, discover endpoints or
    /// select credentials. The caller must enforce that independent binding.
    ///
    /// # Errors
    /// `.LivyService` requires native; a Fabric binding is never a fallback.
    pub fn bind(&self, service: LivyService) -> Result<ExplicitResolver, ReadError> {
        let target = match self.invocation.path.owner.kind {
            OwnerKind::Lakehouse => ResolvedTarget::Lakehouse(service),
            OwnerKind::LivyService if service.profile() == ServiceProfile::NativeWildfire => {
                ResolvedTarget::LivyService(service)
            }
            _ => return Err(ReadError::new(ReadErrorKind::WrongOwner)),
        };
        ExplicitResolver::new(self.invocation.path.owner.clone(), target)
    }

    /// Bind an explicit Fabric endpoint and GUID-typed Lakehouse path.
    ///
    /// Human-name resolution and arbitrary native authorization are not installed.
    /// The caller must use the same endpoint in its Fabric executor and inject its
    /// existing account/tenant-bound credential selection.
    ///
    /// # Errors
    /// Rejects non-Fabric endpoints, native owners and non-GUID selectors before I/O.
    pub fn bind_fabric(&self, endpoint: &EndpointBinding) -> Result<ExplicitResolver, ReadError> {
        if self.invocation.path.owner.kind != OwnerKind::Lakehouse {
            return Err(ReadError::new(ReadErrorKind::UnsupportedCapability));
        }
        if endpoint.origin() != ServiceOrigin::Fabric {
            return Err(ReadError::new(ReadErrorKind::WrongOwner));
        }
        let owner = &self.invocation.path.owner;
        let identity = ItemIdentity::new(
            owner
                .workspace
                .strip_suffix(".Workspace")
                .ok_or_else(|| ReadError::new(ReadErrorKind::InvalidInput))?,
            owner
                .artifact
                .strip_suffix(".Lakehouse")
                .ok_or_else(|| ReadError::new(ReadErrorKind::InvalidInput))?,
        )
        .map_err(|_| ReadError::new(ReadErrorKind::InvalidInput))?;
        self.bind(LivyService::fabric_versioned(
            NativeEndpoint::new(endpoint.base_url())?,
            identity.workspace_id(),
            identity.item_id(),
            "v1",
            FABRIC_API_VERSION,
        )?)
    }

    /// Compose this leaf with the existing explicit Fabric identity and HTTP host.
    ///
    /// # Errors
    /// Rejects inconsistent endpoint/audience/owner bindings without credentials or HTTP.
    pub fn fabric(
        self,
        context: RequestContext,
        http: HttpExecutor,
    ) -> Result<FabricList, ReadError> {
        let resolver = self.bind_fabric(context.endpoint())?;
        let trace = ExecutionTrace::default();
        let executor = NativeFabricExecutor::new(context, http.with_trace(trace.clone()))?;
        Ok(FabricList {
            prepared: self,
            resolver,
            executor,
            trace,
        })
    }
}

/// One Fabric-profile leaf composition; no new credential or execution engine.
pub struct FabricList {
    prepared: PreparedList,
    resolver: ExplicitResolver,
    executor: NativeFabricExecutor,
    trace: ExecutionTrace,
}

impl FabricList {
    /// Consume one collection read through the shared host under the original deadline.
    ///
    /// # Errors
    /// Retains actual pre-stream HTTP status even when streaming or bounded GET
    /// retries fail. An observed status is not evidence that a read completed.
    pub async fn acquire(
        self,
        destination: &Destination,
        deadline: Instant,
        cancelled: impl Future<Output = ()>,
    ) -> Result<PendingRead, HostError> {
        host::acquire(
            &self.prepared.invocation,
            &self.executor,
            &self.resolver,
            self.prepared.options,
            destination,
            deadline,
            cancelled,
        )
        .await
        .map_err(|mut error| {
            error.http_status = error.http_status.or(self.trace.snapshot().status);
            error
        })
    }
}

fn owns(invocation: &ParsedInvocation) -> bool {
    invocation.operation == ReadOperation::List && collection(&invocation.path)
}

fn collection(path: &TypedPath) -> bool {
    matches!(
        path.owner.kind,
        OwnerKind::Lakehouse | OwnerKind::LivyService
    ) && path.facet == ReadFacet::Sessions
}

/// The existing list option schema, with leaf-specific meaning and limitations.
pub fn command() -> Command {
    command_schema()
        .find_subcommand("list")
        .expect("runtime list is registered")
        .clone()
        .about("Read one classic Livy session page for an explicitly bound Lakehouse or LivyService")
        .after_help(
            "Experimental typed paths: /<workspace>.Workspace/<lakehouse>.Lakehouse/sessions\n\
             or /<workspace>.Workspace/<service>.LivyService/sessions.\n\
             One provider page only; no history, readiness, new session or automatic continuation.\n\
             --profile, --project, --timeout and --continuation remain unsupported.\n\
             Native execution requires a caller-authorized native executor; never a Fabric token fallback.",
        )
}

fn routing_matches(args: &[OsString]) -> Option<ArgMatches> {
    if args.first().and_then(|value| value.to_str()) != Some("list") {
        return None;
    }
    command()
        .disable_help_flag(true)
        .arg(
            Arg::new("leaf-help")
                .long("help")
                .short('h')
                .action(ArgAction::SetTrue),
        )
        .mut_arg("path", |arg| {
            arg.required(false).required_unless_present("leaf-help")
        })
        .try_get_matches_from(args)
        .ok()
}

/// Claim only `list` of a typed Livy collection, or that leaf's local help.
///
/// The shared clap schema distinguishes positional paths from option values.
/// Known-but-unsupported options still reach the leaf's explicit capability error.
/// Foreign owners, individual sessions, facades and other commands are untouched.
pub fn claims(args: &[OsString]) -> bool {
    routing_matches(args).is_some_and(|matches| {
        matches.get_one::<String>("path").map_or_else(
            || matches.get_flag("leaf-help"),
            |path| TypedPath::parse(path).is_ok_and(|path| collection(&path)),
        )
    })
}

/// Whether a claimed invocation requests local help, without configuration access.
pub fn help_requested(args: &[OsString]) -> bool {
    claims(args) && routing_matches(args).is_some_and(|matches| matches.get_flag("leaf-help"))
}
