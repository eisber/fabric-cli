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

/// Actual shortcut update output and independent create/delete response evidence.
#[derive(Clone, Debug, Default)]
pub struct ShortcutUpdateResult {
    /// Existing source output message, if the update sequence returned.
    pub outputs: Vec<fabric_cli_output::OutputInput>,
    /// Actual create/update response, including explicit 202 acceptance.
    pub created: Option<ResourceOutcome>,
    /// Original shortcut deletion response when the source name changed.
    pub deleted: Option<ResourceOutcome>,
    /// The user declined after the source metadata GET, without writes.
    pub declined: bool,
}

/// Shortcut update failure, retaining any earlier create/update response.
#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutUpdateError {
    /// Source command error with the failing operation's reconciliation evidence.
    pub error: Box<MutationError>,
    /// Actual prior create/update response, if deletion subsequently failed.
    pub created: Option<ResourceOutcome>,
}

impl std::fmt::Display for ShortcutUpdateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}
impl std::error::Error for ShortcutUpdateError {}

/// Fabric-audience shortcut commands; never use storage token/execution fallback.
pub struct OneLakeShortcutHost<'a> {
    /// Shared Fabric operation policy, with explicitly injected Fabric credential.
    pub operations: &'a mut dyn ResourceOperations,
    /// Existing confirmation/progress/output boundary.
    pub ui: &'a mut dyn MutationUi,
}

impl OneLakeShortcutHost<'_> {
    /// Execute source shortcut SET using the same metadata replacement policy.
    ///
    /// Reads before confirmation, removes service-only `target.type`, and uses
    /// OverwriteOnly for unchanged names. Renaming creates first, then removes
    /// the original with force/verbose=false, exactly as the pinned source does.
    /// Acceptance is retained separately: a source "updated" message is not proof
    /// that an accepted create/delete finished.
    ///
    /// # Errors
    /// Validation/metadata/operation errors remain explicit. A failed later
    /// deletion retains the prior creation response and does not invent rollback.
    pub async fn update(
        &mut self,
        path: &ShortcutPath,
        query: &str,
        input: &str,
        force: bool,
    ) -> Result<ShortcutUpdateResult, ShortcutUpdateError> {
        let mut created = None;
        let result = async {
            crate::set::validate_metadata_query(query, &[])?;
            let read = ShortcutMutations::get(self.operations, path).await?;
            let ResourceOutcome::Completed {
                data: Some(metadata),
                ..
            } = read
            else {
                return Err(MutationError::new(
                    "InvalidResponse",
                    "Shortcut metadata is not complete",
                ));
            };
            if !metadata.is_object() {
                return Err(MutationError::new(
                    "InvalidResponse",
                    "Shortcut metadata is not an object",
                ));
            }
            let current = metadata.get("name").cloned().unwrap_or(json!(""));
            self.ui.notice(
                NoticeKind::Warning,
                "Modifying properties may lead to unintended consequences",
            );
            if !force && !self.ui.confirm("Are you sure?").await {
                return Ok(ShortcutUpdateResult {
                    declined: true,
                    ..ShortcutUpdateResult::default()
                });
            }
            let mut updated = crate::set::update_full_element(metadata, query, input)?;
            if let Some(target) = updated.get_mut("target") {
                match target {
                    Value::Object(target) => {
                        target.remove("type");
                    }
                    Value::Array(values) if !values.iter().any(|value| value == "type") => {}
                    Value::String(value) if !value.contains("type") => {}
                    _ => {
                        return Err(MutationError::new(
                            "InvalidQuery",
                            "Shortcut target cannot remove its service type field",
                        ));
                    }
                }
            }
            let renamed = updated.get("name").cloned().unwrap_or(json!("")) != current;
            let outcome = ShortcutMutations::create(
                self.operations,
                path.workspace(),
                path.item(),
                updated,
                if renamed {
                    ShortcutConflictPolicy::Abort
                } else {
                    ShortcutConflictPolicy::OverwriteOnly
                },
            )
            .await?;
            created = Some(outcome.clone());
            let deleted = if renamed {
                Some(ShortcutMutations::delete(self.operations, path).await?)
            } else {
                None
            };
            Ok(ShortcutUpdateResult {
                outputs: vec![message("Shortcut updated")],
                created: Some(outcome),
                deleted,
                declined: false,
            })
        }
        .await;
        result.map_err(|error| ShortcutUpdateError {
            error: Box::new(error),
            created,
        })
    }

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
