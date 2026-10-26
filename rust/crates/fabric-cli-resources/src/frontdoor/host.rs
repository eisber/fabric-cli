// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{AccessCommand, AccessKind};
use crate::{
    AclRead, ApiOptions, CommandError, CommandValue, FamilyPresentation, NamedResource,
    acl_resource, api, find, list_local_labels, read_api_input, read_named_acls,
    render_family_error, render_family_output,
};
use fabric_cli_context::{Element, Lookup, PathInput, ResolveOptions, Resolver};
use fabric_cli_output::{
    Timestamp,
    text::{CharacterWidth, TextError, TextPlan},
};
use fabric_sdk_access::AccessClient;
use fabric_sdk_api::{ApiClient, Limits, Origin, RequestBounds, cancellable};
use fabric_sdk_catalog::CatalogClient;
use std::{
    future::Future,
    io::Write,
    path::Path,
    pin::{Pin, pin},
    sync::Arc,
};

/// Existing context/session integration. Implementations resolve a command-local
/// snapshot; they do not mutate a global current directory or select an identity.
pub trait AccessTargets {
    /// Resolve original path tokens on the caller's runtime.
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Arc<Element>, CommandError>> + Send + 'a>>;
}

impl<L: Lookup + Sync + ?Sized> AccessTargets for Resolver<'_, L> {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Arc<Element>, CommandError>> + Send + 'a>> {
        Box::pin(async move {
            let tokens: Vec<_> = path.iter().map(String::as_str).collect();
            Resolver::resolve(self, PathInput::Tokens(&tokens), ResolveOptions::default())
                .map_err(|error| CommandError::new(&error.code, error.message))
        })
    }
}

/// Caller-bound SDK sessions. Power BI intentionally uses Fabric credentials,
/// but still has its own explicit origin selection in each raw request.
pub struct ApiClients<'a> {
    /// Fabric session used for ACL, catalog and default raw API requests.
    pub fabric: &'a ApiClient,
    /// Storage-scoped session, only when explicitly configured.
    pub onelake: Option<&'a ApiClient>,
    /// ARM-scoped session, only when explicitly configured.
    pub arm: Option<&'a ApiClient>,
    /// Explicit Power BI binding using the pinned source Fabric scope.
    pub powerbi: Option<&'a ApiClient>,
}

impl ApiClients<'_> {
    fn get(&self, origin: Origin) -> Result<&ApiClient, CommandError> {
        let client = match origin {
            Origin::Fabric => Some(self.fabric),
            Origin::OneLake => self.onelake,
            Origin::Arm => self.arm,
            Origin::PowerBi => self.powerbi,
        };
        client.ok_or_else(|| CommandError::new("NotConfigured", "API audience is not configured"))
    }
}

/// Borrowed command host. No runtime, token discovery, terminal or HTTP engine
/// is constructed. Files are only opened through explicit configured arguments.
pub struct AccessHost<'a> {
    /// Existing caller-credential-bound SDK sessions.
    pub clients: ApiClients<'a>,
    /// Existing host resolver; local definitions and API do not invoke it.
    pub targets: &'a dyn AccessTargets,
    /// Isolated configured local definition file, never discovered in home.
    pub label_definitions: Option<&'a Path>,
    /// Explicit validated framing boundary for a source multipart upload.
    pub multipart_boundary: &'a str,
    /// Existing collection/response bounds.
    pub limits: Limits,
    /// Existing raw request allocation bounds, validated before credentials.
    pub request_bounds: RequestBounds,
}

impl AccessHost<'_> {
    /// Executes the typed leaf through the already shared SDK/CLI consumers.
    pub async fn execute(
        &mut self,
        command: &AccessCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CommandValue, CommandError> {
        if command.skill.is_some() {
            return Err(CommandError::new(
                "NotSupported",
                "Skill headers require a configured shared host binding",
            ));
        }
        let mut cancelled = pin!(cancelled);
        match &command.kind {
            AccessKind::AclList { path, query, .. } | AccessKind::AclGet { path, query, .. } => {
                if matches!(
                    &command.kind,
                    AccessKind::AclGet {
                        output: Some(_),
                        ..
                    }
                ) {
                    return Err(CommandError::new(
                        "NotSupported",
                        "ACL export requires a configured export host",
                    ));
                }
                let context = cancellable(self.targets.resolve(path), cancelled.as_mut())
                    .await
                    .map_err(CommandError::from)??;
                let target = NamedResource {
                    resource: acl_resource(&context)?,
                    name: context.name(),
                };
                let options = match &command.kind {
                    AccessKind::AclList { long, .. } => AclRead::List {
                        long: *long,
                        query: query.clone(),
                    },
                    _ => AclRead::Get {
                        query: query.clone(),
                    },
                };
                read_named_acls(
                    &AccessClient::new(self.clients.fabric.clone()),
                    &target,
                    context.workspace().as_ref().map(|ws| ws.name()).as_deref(),
                    options,
                    self.limits,
                    cancelled.as_mut(),
                )
                .await
            }
            AccessKind::LabelListLocal => cancellable(
                async { list_local_labels(self.label_definitions) },
                cancelled.as_mut(),
            )
            .await
            .map_err(CommandError::from)?,
            AccessKind::Find(options) => {
                find(
                    &CatalogClient::new(self.clients.fabric.clone()),
                    options,
                    self.limits,
                    cancelled.as_mut(),
                )
                .await
            }
            AccessKind::Api(options) => {
                let client = self.clients.get(options.origin)?;
                let input = read_api_input(
                    &options.input,
                    self.multipart_boundary,
                    self.request_bounds.max_body_bytes,
                )?;
                let options = ApiOptions {
                    origin: options.origin,
                    method: options.method,
                    endpoint: options.endpoint.clone(),
                    params: options.params.clone(),
                    headers: options.headers.clone(),
                    input,
                    query: options.query.clone(),
                    show_headers: options.show_headers,
                    bounds: self.request_bounds,
                };
                api(
                    client,
                    &options,
                    self.limits.max_response_bytes,
                    cancelled.as_mut(),
                )
                .await
            }
        }
    }
}

/// Shared-renderer result and explicit logger observations.
pub struct AccessOutput {
    /// Existing ordered stdout/stderr records and source exit code.
    pub plan: TextPlan,
    /// Host logger warnings, not silently redirected into JSON.
    pub warnings: Vec<String>,
}

impl AccessOutput {
    /// Writes caller-owned streams; a failed write is never a successful exit.
    pub fn write_to(
        &self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> std::io::Result<u8> {
        self.plan.write_to(stdout, stderr)?;
        Ok(self.plan.exit_code)
    }
}

/// Uses the existing family renderer for both successful and failing execution.
/// The command selects its own surface and optional output-format override.
pub fn render(
    command: &AccessCommand,
    result: Result<CommandValue, CommandError>,
    timestamp: Timestamp,
    presentation: &FamilyPresentation<'_>,
    widths: &impl CharacterWidth,
) -> Result<AccessOutput, TextError> {
    let mut presentation = presentation.clone();
    presentation.surface = command.surface();
    if let Some(format) = command.output_format {
        presentation.format = format;
    }
    let (plan, warnings) = match result {
        Ok(value) => (
            render_family_output(
                &value,
                command.command_name(),
                timestamp,
                &presentation,
                widths,
            )?,
            value.warnings,
        ),
        Err(error) => (
            render_family_error(&error, command.command_name(), timestamp, &presentation)?,
            error.warnings,
        ),
    };
    Ok(AccessOutput { plan, warnings })
}
