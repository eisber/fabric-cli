// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{MutationError, MutationResult, MutationUi, NoticeKind, message};
use fabric_cli_context::ItemType;
use fabric_sdk_onelake::{
    OneLakePath,
    shortcuts::{ShortcutConflictPolicy, ShortcutMutations, ShortcutPath},
};
use fabric_sdk_workspaces::mutations::{ResourceError, ResourceOperations, ResourceOutcome};
use serde_json::{Value, json};

/// Source `ln`/`mklink` arguments after the existing host resolves a target path.
#[derive(Clone, Debug)]
pub struct ShortcutCreateOptions {
    /// Explicit resolved OneLake target, never ambient lookup in this library.
    pub target: Option<OneLakePath>,
    /// Source argparse-style JSON input tokens, mutually exclusive with target.
    pub input: Option<Vec<String>>,
    /// Exact source target type; `--target` requires case-sensitive `oneLake`.
    pub shortcut_type: String,
    /// Bypass confirmation and select GenerateUniqueName, not overwrite.
    pub force: bool,
}

impl Default for ShortcutCreateOptions {
    fn default() -> Self {
        Self {
            target: None,
            input: None,
            shortcut_type: "oneLake".into(),
            force: false,
        }
    }
}

/// Existing hierarchy host's shortcut-creation context, not a DFS file path.
#[derive(Clone, Debug)]
pub struct ShortcutCreateDestination {
    /// Selected owning workspace identifier.
    pub workspace: String,
    /// Selected owning item identifier.
    pub item: String,
    /// Original context-local path, preserving source extension behavior.
    pub local_path: String,
    /// Original display path, including the requested Shortcut suffix.
    pub display_path: String,
    /// Source item type, supplied by the existing context host.
    pub item_type: ItemType,
}

/// Fabric-audience shortcut commands; never use storage token/execution fallback.
pub struct OneLakeShortcutHost<'a> {
    /// Shared Fabric operation policy, with explicitly injected Fabric credential.
    pub operations: &'a mut dyn ResourceOperations,
    /// Existing confirmation/progress/output boundary.
    pub ui: &'a mut dyn MutationUi,
}

impl OneLakeShortcutHost<'_> {
    /// Execute source `ln`/`mklink` with a resolved OneLake or JSON target.
    ///
    /// # Errors
    /// Preserves prompt-before-validation, Lakehouse/Files-or-Tables/suffix checks,
    /// exclusive input and target-type checks, source name validation and JSON
    /// token normalization. Unsupported JSON-domain values fail before dispatch.
    pub async fn create(
        &mut self,
        destination: &ShortcutCreateDestination,
        options: &ShortcutCreateOptions,
    ) -> Result<MutationResult, MutationError> {
        if !options.force && !self.ui.confirm("Are you sure?").await {
            return Ok(MutationResult {
                declined: true,
                ..MutationResult::default()
            });
        }
        if destination.item_type.as_str() != "Lakehouse" {
            return Err(MutationError::new(
                "InvalidPath",
                "Only supported within Lakehouse",
            ));
        }
        let local = destination.local_path.as_str();
        if !local.to_lowercase().starts_with("files/")
            && !local.to_lowercase().starts_with("tables/")
        {
            return Err(MutationError::new(
                "InvalidPath",
                "Only supported within Files/ and Tables/",
            ));
        }
        if !destination
            .display_path
            .to_lowercase()
            .ends_with(".shortcut")
        {
            return Err(MutationError::new(
                "InvalidPath",
                "Invalid path. The path must end with '.Shortcut'",
            ));
        }
        if options.target.is_some() == options.input.is_some() {
            return Err(MutationError::new(
                "InvalidInput",
                "Invalid arguments. Exactly one of --target or --input is required",
            ));
        }
        if options.target.is_some() && options.shortcut_type != "oneLake" {
            return Err(MutationError::new(
                "InvalidInput",
                "Invalid arguments. --target is only supported for oneLake",
            ));
        }
        let full = without_extension(local).trim_end_matches('/');
        let (path, name) = full.rsplit_once('/').unwrap_or(("", full));
        validate_name(name)?;
        let target = if let Some(target) = &options.target {
            json!({"workspaceId":target.workspace(),"itemId":target.item(),"path":target.relative()})
        } else {
            let tokens = options
                .input
                .as_ref()
                .ok_or_else(|| MutationError::new("InvalidInput", "Missing shortcut input"))?;
            let content = tokens
                .join(" ")
                .trim_matches(['"', '\''])
                .replace('\'', "\"");
            fabric_cli_output::parse_data(&content)
                .map_err(|_| MutationError::new("InvalidJson", "Invalid JSON format"))?
        };
        let mut targets = serde_json::Map::new();
        targets.insert(options.shortcut_type.clone(), target);
        self.ui
            .notice(NoticeKind::Progress, "Creating a new Shortcut...");
        let outcome = ShortcutMutations::create(
            self.operations,
            &destination.workspace,
            &destination.item,
            json!({"path":path,"name":name,"target":targets}),
            if options.force {
                ShortcutConflictPolicy::GenerateUniqueName
            } else {
                ShortcutConflictPolicy::Abort
            },
        )
        .await?;
        let mut result = MutationResult {
            outcome: Some(outcome.clone()),
            ..MutationResult::default()
        };
        if let ResourceOutcome::Completed {
            status: status @ (200 | 201),
            data,
        } = outcome
        {
            let Some(data) = data.filter(Value::is_object) else {
                return Err(invalid_created_metadata(status));
            };
            let name = match data.get("name") {
                None | Some(Value::Null) => "None",
                Some(Value::String(name)) => name,
                _ => return Err(invalid_created_metadata(status)),
            };
            result
                .outputs
                .push(message(format!("'{name}.Shortcut' created")));
        }
        Ok(result)
    }

    /// Delete actual shortcut metadata with source force/verbose semantics.
    ///
    /// # Errors
    /// Keeps accepted/completed responses separate and propagates the Fabric
    /// operation error, rather than deleting the underlying storage recursively.
    pub async fn remove(
        &mut self,
        path: &ShortcutPath,
        display_name: &str,
        force: bool,
        verbose: bool,
    ) -> Result<MutationResult, MutationError> {
        if !force && !self.ui.confirm("Are you sure?").await {
            if verbose {
                self.ui
                    .notice(NoticeKind::Warning, "Resource delete cancelled");
            }
            return Ok(MutationResult {
                declined: true,
                ..MutationResult::default()
            });
        }
        if verbose {
            if force {
                self.ui
                    .notice(NoticeKind::Warning, "Executing force delete...");
            }
            self.ui.notice(
                NoticeKind::Progress,
                &format!("Deleting '{display_name}'..."),
            );
        }
        let outcome = ShortcutMutations::delete(self.operations, path).await?;
        let text = if verbose {
            match &outcome {
                ResourceOutcome::Completed {
                    status: 200 | 201, ..
                } => Some(format!("'{display_name}' deleted")),
                ResourceOutcome::Accepted { .. } => {
                    Some(format!("'{display_name}' is being deleted"))
                }
                _ => None,
            }
        } else {
            None
        };
        Ok(MutationResult {
            outputs: text.map(|text| vec![message(text)]).unwrap_or_default(),
            outcome: Some(outcome),
            ..MutationResult::default()
        })
    }
}

fn invalid_created_metadata(status: u16) -> MutationError {
    let mut error = ResourceError::input();
    error.code = "InvalidResponse".into();
    error.message = "Shortcut creation returned invalid metadata".into();
    error.status = Some(status);
    error.mutation_may_have_occurred = true;
    error.into()
}

fn validate_name(name: &str) -> Result<(), MutationError> {
    if name.contains(['"', '\\', ':', '|', '<', '>', '*', '?', '.', '%', '+']) {
        return Err(MutationError::new(
            "InvalidPath",
            r#"Invalid shortcut name. The name should not include any of the following characters: [\"\\:|&lt;&gt;*?.%+]"#,
        ));
    }
    Ok(())
}

fn without_extension(path: &str) -> &str {
    let start = path.rfind('/').map_or(0, |index| index + 1);
    let Some(dot) = path[start..].rfind('.').map(|index| index + start) else {
        return path;
    };
    if path[start..dot].chars().any(|character| character != '.') {
        &path[..dot]
    } else {
        path
    }
}
