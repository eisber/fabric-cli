// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ACL/label execution over explicit session, resolver and confirmation inputs.

use crate::{
    AclRead, CommandError, CommandValue, Confirmation, MutationOptions, NamedResource,
    acl_resource, list_local_labels, mutate_acl, mutate_label, parser::AccessKind, read_named_acls,
};
use fabric_cli_command_contracts::parser::ParsedCommand;
use fabric_cli_context::{Element, Lookup, NodeKind, PathInput, ResolveOptions, Resolver};
use fabric_cli_output::text::OutputFormat;
use fabric_sdk_access::{AccessClient, LabelItem};
use fabric_sdk_api::{ApiClient, Limits, cancellable};
use std::{
    future::Future,
    path::Path,
    pin::{Pin, pin},
    sync::Arc,
};

/// Existing command-local resolver; it never changes the global directory.
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

fn unbound_skill() -> CommandError {
    CommandError::new(
        "NotSupported",
        "Skill headers require a configured shared host binding",
    )
}

/// Executes the local-only leaf before constructing remote SDK composition.
pub fn execute_local(
    command: &ParsedCommand<AccessKind>,
    definitions: Option<&Path>,
) -> Option<Result<CommandValue, CommandError>> {
    if !matches!(command.kind, AccessKind::LabelListLocal) {
        return None;
    }
    Some(if command.skill.is_some() {
        Err(unbound_skill())
    } else {
        list_local_labels(definitions)
    })
}

/// Borrowed access capabilities; missing confirmation is never implicit consent.
pub struct AccessHost<'a> {
    /// Caller-selected Fabric session.
    pub client: &'a ApiClient,
    /// Existing command-local resolver.
    pub targets: &'a dyn AccessTargets,
    /// Explicit local definition path, never discovered in home.
    pub label_definitions: Option<&'a Path>,
    /// Asynchronous caller-owned confirmation.
    pub confirmation: Option<&'a mut dyn Confirmation>,
    /// Existing finite collection and execution bounds.
    pub limits: Limits,
}

impl AccessHost<'_> {
    /// Uses v2 record-valued unqueried text GET without changing JSON/query rules.
    pub async fn execute_for_output(
        &mut self,
        command: &ParsedCommand<AccessKind>,
        format: OutputFormat,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CommandValue, CommandError> {
        if format == OutputFormat::Text
            && let AccessKind::AclGet { query: None, .. } = &command.kind
        {
            let mut records = command.clone();
            if let AccessKind::AclGet { query, .. } = &mut records.kind {
                *query = Some(".".into());
            }
            return self.execute(&records, cancelled).await;
        }
        self.execute(command, cancelled).await
    }

    /// Executes the typed leaf without discovering a runtime, provider or UI.
    pub async fn execute(
        &mut self,
        command: &ParsedCommand<AccessKind>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CommandValue, CommandError> {
        if command.skill.is_some() {
            return Err(unbound_skill());
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
                    &AccessClient::new(self.client.clone()),
                    &target,
                    context.workspace().as_ref().map(|ws| ws.name()).as_deref(),
                    options,
                    self.limits,
                    cancelled.as_mut(),
                )
                .await
            }
            AccessKind::LabelListLocal => cancellable(
                async { execute_local(command, self.label_definitions).expect("local leaf") },
                cancelled.as_mut(),
            )
            .await
            .map_err(CommandError::from)?,
            AccessKind::AclChange {
                path,
                change,
                force,
            } => {
                let context = cancellable(self.targets.resolve(path), cancelled.as_mut())
                    .await
                    .map_err(CommandError::from)??;
                let target = NamedResource {
                    resource: acl_resource(&context)?,
                    name: context.name(),
                };
                let mut missing = MissingConfirmation;
                let prompt = self.confirmation.as_deref_mut().unwrap_or(&mut missing);
                mutate_acl(
                    &AccessClient::new(self.client.clone()),
                    &target,
                    change,
                    MutationOptions {
                        force: *force,
                        limits: self.limits,
                    },
                    prompt,
                    cancelled.as_mut(),
                )
                .await
            }
            AccessKind::LabelChange {
                path,
                change,
                force,
            } => {
                let context = cancellable(self.targets.resolve(path), cancelled.as_mut())
                    .await
                    .map_err(CommandError::from)??;
                let NodeKind::Item(kind) = context.kind() else {
                    return Err(CommandError::new(
                        "UnsupportedCommand",
                        "Resource does not support sensitivity labels",
                    ));
                };
                let item = LabelItem {
                    id: context
                        .id()
                        .ok_or_else(|| {
                            CommandError::new(
                                "NotFound",
                                "The requested resource could not be found",
                            )
                        })?
                        .into(),
                    item_type: kind.as_str().into(),
                };
                let mut missing = MissingConfirmation;
                let prompt = self.confirmation.as_deref_mut().unwrap_or(&mut missing);
                mutate_label(
                    &AccessClient::new(self.client.clone()),
                    &item,
                    change,
                    self.label_definitions,
                    MutationOptions {
                        force: *force,
                        limits: self.limits,
                    },
                    prompt,
                    cancelled.as_mut(),
                )
                .await
            }
        }
    }
}

struct MissingConfirmation;
impl Confirmation for MissingConfirmation {
    fn confirm<'a>(
        &'a mut self,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, CommandError>> + Send + 'a>> {
        Box::pin(async {
            Err(CommandError::new(
                "NotConfigured",
                "Confirmation is not configured; use an explicit force flag to skip the prompt",
            ))
        })
    }
}
