// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Local Notebook loading, preparation, and SDK-backed native deployment.

use crate::transforms::NotebookTransforms;
use fabric_sdk::{
    Client,
    notebooks::{
        DefinitionPart, NotebookDefinition, NotebookError, NotebookExecutor, NotebookLimits,
    },
};
use serde_json::Value;
use std::{
    fmt, fs,
    future::Future,
    io::{self, Read},
    num::NonZeroUsize,
    path::Path,
};

/// Explicit native deployment failure, never a success-shaped skipped artifact.
#[derive(Debug)]
pub enum DeploymentError {
    /// Local file read failed, with the underlying I/O error retained.
    Io(io::Error),
    /// Metadata, definition format, or caller input was invalid.
    InvalidInput,
    /// Content format/files are outside the supported single-Notebook slice.
    UnsupportedContent,
    /// A symlink, directory or special file is not a regular definition file.
    UnsafeFile,
    /// Input/output exceeds the caller's allocation bound.
    Limit,
    /// A referenced logical item has not been deployed.
    UndeployedLogicalId,
    /// More than one deployed Notebook has the exact selected name.
    AmbiguousTarget,
    /// SDK service failure with mutation/operation reconciliation evidence.
    Service(Box<NotebookError>),
    /// Parameter preparation failed with original typed bounded-work evidence.
    Transform(Box<crate::parameter_transforms::TransformError>),
}

impl fmt::Display for DeploymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Io(_) => "could not read the local Notebook definition",
            Self::InvalidInput => "invalid Notebook deployment input or metadata",
            Self::UnsupportedContent => "unsupported Notebook content in this native slice",
            Self::UnsafeFile => "Notebook definition requires regular non-symlink files",
            Self::Limit => "Notebook definition exceeded its byte bound",
            Self::UndeployedLogicalId => "a referenced logical item is not deployed",
            Self::AmbiguousTarget => "multiple deployed Notebooks match the exact name",
            Self::Service(_) => "Notebook deployment failed or is incomplete",
            Self::Transform(_) => "Notebook parameter preparation failed",
        })
    }
}

impl std::error::Error for DeploymentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Service(error) => Some(error.as_ref()),
            Self::Transform(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// Limits for local files and prepared output, enforced before service mutation.
#[derive(Clone, Copy, Debug)]
pub struct DefinitionLimits {
    /// Maximum aggregate UTF-8 bytes across all input and transformed files.
    pub max_bytes: NonZeroUsize,
}

impl Default for DefinitionLimits {
    fn default() -> Self {
        Self {
            max_bytes: NonZeroUsize::new(10 * 1024 * 1024).unwrap(),
        }
    }
}

/// Loaded, bounded local content; it has not contacted Fabric.
///
/// Content is intentionally excluded from Debug to avoid exposing Notebook data.
pub struct LocalNotebook {
    name: String,
    description: String,
    logical_id: String,
    files: Vec<(String, String)>,
    ipynb: bool,
    limits: DefinitionLimits,
}

impl LocalNotebook {
    /// Reads a single Fabric Notebook directory synchronously.
    ///
    /// Supports `.platform` and exactly one `notebook-content.py` or
    /// `notebook-content.ipynb`, plus optional Git `notebook-settings.json`.
    /// Other files are explicitly unsupported, not silently omitted.
    /// Call outside an async worker, or use the caller's blocking-I/O facility.
    ///
    /// # Errors
    /// Rejects malformed metadata/content, symlinks/special files, excess bytes,
    /// mixed formats, and unsupported nested/assets/config files.
    pub fn load(directory: &Path, limits: DefinitionLimits) -> Result<Self, DeploymentError> {
        let metadata = fs::symlink_metadata(directory).map_err(DeploymentError::Io)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(DeploymentError::UnsafeFile);
        }
        let mut remaining = limits.max_bytes.get();
        let mut files = Vec::new();
        for entry in fs::read_dir(directory).map_err(DeploymentError::Io)? {
            let entry = entry.map_err(DeploymentError::Io)?;
            let file_type = entry.file_type().map_err(DeploymentError::Io)?;
            if !file_type.is_file() || file_type.is_symlink() {
                return Err(DeploymentError::UnsafeFile);
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| DeploymentError::UnsupportedContent)?;
            if !matches!(
                name.as_str(),
                ".platform"
                    | "notebook-content.py"
                    | "notebook-content.ipynb"
                    | "notebook-settings.json"
            ) {
                return Err(DeploymentError::UnsupportedContent);
            }
            let mut bytes = Vec::new();
            fs::File::open(entry.path())
                .map_err(DeploymentError::Io)?
                .take((remaining as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(DeploymentError::Io)?;
            if bytes.len() > remaining {
                return Err(DeploymentError::Limit);
            }
            remaining -= bytes.len();
            let text = String::from_utf8(bytes).map_err(|_| DeploymentError::UnsupportedContent)?;
            // Python Path.read_text uses universal newline handling.
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            files.push((name, text));
        }
        Self::from_text_files(files, limits)
    }

    /// Validate bounded caller-injected files using the same native Notebook codec.
    ///
    /// Filesystem adapters must reject links/special files before supplying bytes.
    /// This does not bypass format, duplicate-path, metadata or allocation checks.
    ///
    /// # Errors
    /// Rejects unsupported/duplicate paths, invalid UTF-8/metadata and excess bytes.
    pub fn from_files(
        files: Vec<(String, Vec<u8>)>,
        limits: DefinitionLimits,
    ) -> Result<Self, DeploymentError> {
        let mut remaining = limits.max_bytes.get();
        let mut text_files = Vec::new();
        for (name, bytes) in files {
            if !matches!(
                name.as_str(),
                ".platform"
                    | "notebook-content.py"
                    | "notebook-content.ipynb"
                    | "notebook-settings.json"
            ) || text_files.iter().any(|(path, _)| path == &name)
            {
                return Err(DeploymentError::UnsupportedContent);
            }
            remaining = remaining
                .checked_sub(bytes.len())
                .ok_or(DeploymentError::Limit)?;
            let text = String::from_utf8(bytes).map_err(|_| DeploymentError::UnsupportedContent)?;
            text_files.push((name, text.replace("\r\n", "\n").replace('\r', "\n")));
        }
        Self::from_text_files(text_files, limits)
    }

    fn from_text_files(
        mut files: Vec<(String, String)>,
        limits: DefinitionLimits,
    ) -> Result<Self, DeploymentError> {
        files.sort_by_key(|(name, _)| match name.as_str() {
            ".platform" => 0,
            "notebook-content.py" | "notebook-content.ipynb" => 1,
            _ => 3,
        });
        let platform = files
            .iter()
            .find(|(name, _)| name == ".platform")
            .ok_or(DeploymentError::InvalidInput)?;
        let metadata: Value =
            serde_json::from_str(&platform.1).map_err(|_| DeploymentError::InvalidInput)?;
        let name = metadata
            .pointer("/metadata/displayName")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or(DeploymentError::InvalidInput)?
            .to_owned();
        if metadata.pointer("/metadata/type").and_then(Value::as_str) != Some("Notebook")
            || metadata
                .pointer("/config/logicalId")
                .and_then(Value::as_str)
                .is_none_or(|id| id.trim().is_empty())
        {
            return Err(DeploymentError::InvalidInput);
        }
        let description = match metadata.pointer("/metadata/description") {
            None => String::new(),
            Some(Value::String(value)) => value.clone(),
            _ => return Err(DeploymentError::InvalidInput),
        };
        let logical_id = metadata
            .pointer("/config/logicalId")
            .and_then(Value::as_str)
            .expect("validated logical ID")
            .to_owned();
        let py = files.iter().any(|(name, _)| name == "notebook-content.py");
        let ipynb = files
            .iter()
            .any(|(name, _)| name == "notebook-content.ipynb");
        if py == ipynb
            || (ipynb
                && files
                    .iter()
                    .any(|(name, _)| name == "notebook-settings.json"))
        {
            return Err(DeploymentError::UnsupportedContent);
        }
        for (name, contents) in &files {
            validate_json_file(name, contents)?;
        }
        Ok(Self {
            name,
            description,
            logical_id,
            files,
            ipynb,
            limits,
        })
    }

    /// Exact `.platform` display name, independent of the directory spelling.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Source logical identifier, including the permitted export placeholder.
    pub fn logical_id(&self) -> &str {
        &self.logical_id
    }

    /// Applies the supported transforms and encodes exact ordered definition parts.
    ///
    /// # Errors
    /// Rejects invalid workspace input, unresolved IDs, invalid transformed JSON
    /// and allocation overflow before service discovery or mutation.
    pub fn prepare(
        &self,
        workspace: &str,
        transforms: &NotebookTransforms,
    ) -> Result<PreparedNotebook, DeploymentError> {
        self.prepare_with(workspace, |_, content, remaining| {
            transforms.apply(content, workspace, remaining)
        })
    }

    /// Prepare through the same codec with a validated bounded parameter program.
    ///
    /// Retains item preprocessing order: logical IDs, key/value rules, find rules,
    /// then default workspace IDs. `.platform` bypasses parameterization.
    ///
    /// # Errors
    /// Returns original parameter domain/cancellation/resource failures before I/O.
    pub fn prepare_parameters(
        &self,
        workspace: &str,
        transforms: &NotebookTransforms,
        program: &crate::parameter_transforms::ParameterProgram,
        budget: &mut crate::parameter_transforms::TransformBudget<'_>,
    ) -> Result<PreparedNotebook, DeploymentError> {
        self.prepare_with(workspace, |path, content, remaining| {
            let content = transforms.logical_content(content, remaining)?;
            let content = program
                .apply(&content, path, budget)
                .map_err(|error| DeploymentError::Transform(Box::new(error)))?;
            crate::transforms::replace_workspace_ids(&content, workspace, remaining)
        })
    }

    fn prepare_with(
        &self,
        workspace: &str,
        mut transform: impl FnMut(&str, &str, usize) -> Result<String, DeploymentError>,
    ) -> Result<PreparedNotebook, DeploymentError> {
        if workspace.trim().is_empty() {
            return Err(DeploymentError::InvalidInput);
        }
        let mut remaining = self.limits.max_bytes.get();
        let mut parts = Vec::new();
        for (path, content) in &self.files {
            let content = if path == ".platform" {
                content.clone()
            } else {
                transform(path, content, remaining)?
            };
            validate_json_file(path, &content)?;
            remaining = remaining
                .checked_sub(content.len())
                .ok_or(DeploymentError::Limit)?;
            parts.push(
                DefinitionPart::inline(path, content.as_bytes())
                    .map_err(|_| DeploymentError::InvalidInput)?,
            );
        }
        Ok(PreparedNotebook {
            workspace: workspace.to_owned(),
            name: self.name.clone(),
            description: self.description.clone(),
            definition: NotebookDefinition {
                format: self.ipynb.then(|| "ipynb".to_owned()),
                parts,
            },
        })
    }
}

fn validate_json_file(path: &str, content: &str) -> Result<(), DeploymentError> {
    if path.ends_with(".json") || path.ends_with(".ipynb") || path == ".platform" {
        let value: Value =
            serde_json::from_str(content).map_err(|_| DeploymentError::InvalidInput)?;
        if !value.is_object() {
            return Err(DeploymentError::InvalidInput);
        }
        if path.ends_with(".ipynb")
            && (!value.get("cells").is_some_and(Value::is_array)
                || value.get("nbformat").and_then(Value::as_u64) != Some(4))
        {
            return Err(DeploymentError::UnsupportedContent);
        }
    }
    Ok(())
}

/// Prepared Notebook input; no secret, ambient state, or runtime is stored.
pub struct PreparedNotebook {
    workspace: String,
    name: String,
    description: String,
    definition: NotebookDefinition,
}

/// The service mutation that actually completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeploymentAction {
    /// A new Notebook was created.
    Created,
    /// An existing Notebook definition and metadata were updated.
    Updated,
}

/// A completed native deployment, never merely an accepted request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentOutcome {
    /// Completed mutation type.
    pub action: DeploymentAction,
    /// Actual deployed item ID.
    pub item_id: String,
}

impl PreparedNotebook {
    /// Publish this definition against a caller-complete repository target snapshot.
    ///
    /// Uses the exact CICD metadata/definition/folder field order and the existing
    /// shared operation engine. It does not discover targets, move folders, replay
    /// uncertain writes, or create another HTTP client/runtime/poller.
    ///
    /// # Errors
    /// Invalid targets fail before dispatch. Accepted/incomplete operations retain
    /// original mutation uncertainty; create requires an actual valid result ID.
    pub async fn publish_with_session<E: fabric_sdk_operations::OperationExecutor>(
        &self,
        session: &mut fabric_sdk_operations::OperationSession<'_, E>,
        existing_item: Option<&str>,
    ) -> Result<DeploymentOutcome, fabric_sdk_operations::OperationError> {
        use crate::deployment_config::valid_guid;
        use fabric_sdk_operations::{FabricOperation, OperationErrorKind};
        use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
        use serde_json::json;

        if !valid_guid(&self.workspace) || existing_item.is_some_and(|id| !valid_guid(id)) {
            return Err(session.evidence(OperationErrorKind::InvalidInput));
        }
        let definition = serde_json::to_value(&self.definition)
            .map_err(|_| session.evidence(OperationErrorKind::InvalidInput))?;
        let (path, body) = if let Some(item) = existing_item {
            (
                format!(
                    "/v1/workspaces/{}/items/{item}/updateDefinition?updateMetadata=True",
                    self.workspace
                ),
                json!({"definition": definition}),
            )
        } else {
            (
                format!("/v1/workspaces/{}/items", self.workspace),
                json!({
                    "displayName": self.name, "type": "Notebook",
                    "description": self.description, "definition": definition, "folderId": "",
                }),
            )
        };
        let operation = FabricOperation::new(
            ResourceMethod::Post,
            &path,
            fabric_core_json::json_dumps(&body).into_bytes(),
            CompletionPolicy::PythonSynchronous,
        )
        .map_err(|kind| session.evidence(kind))?;
        let response = session.execute(operation).await?;
        if response.status == 202 {
            return Err(session.evidence(OperationErrorKind::InvalidResponse));
        }
        let item_id = match existing_item {
            Some(id) => id.to_owned(),
            None => {
                let result: Value = serde_json::from_slice(&response.body)
                    .map_err(|_| session.evidence(OperationErrorKind::InvalidResponse))?;
                result
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_guid(id))
                    .ok_or_else(|| session.evidence(OperationErrorKind::InvalidResponse))?
                    .to_owned()
            }
        };
        Ok(DeploymentOutcome {
            action: if existing_item.is_some() {
                DeploymentAction::Updated
            } else {
                DeploymentAction::Created
            },
            item_id,
        })
    }

    /// Discovers the exact Notebook name, then creates or updates through the SDK.
    ///
    /// Uses only injected execution/credentials/cancellation. Never retries a
    /// mutation or treats incomplete discovery as absence. Dropping/cancelling
    /// the future releases local work but does not roll back a service mutation.
    ///
    /// # Errors
    /// Surfaces ambiguous targets, discovery failures and accepted/incomplete
    /// operation evidence; callers must reconcile before replaying.
    pub async fn deploy(
        &self,
        client: &Client,
        executor: &dyn NotebookExecutor,
        limits: NotebookLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DeploymentOutcome, DeploymentError> {
        let mut session = client.notebook_session(executor, limits, cancelled);
        let items = session
            .list_items(&self.workspace)
            .await
            .map_err(service_error)?;
        let mut matches = items
            .into_iter()
            .filter(|item| item.item_type == "Notebook" && item.display_name == self.name);
        let existing = matches.next();
        if matches.next().is_some() {
            return Err(DeploymentError::AmbiguousTarget);
        }
        if let Some(existing) = existing {
            session
                .update(&self.workspace, &existing.id, &self.definition)
                .await
                .map_err(service_error)?;
            Ok(DeploymentOutcome {
                action: DeploymentAction::Updated,
                item_id: existing.id,
            })
        } else {
            let item = session
                .create(
                    &self.workspace,
                    &self.name,
                    &self.description,
                    &self.definition,
                )
                .await
                .map_err(service_error)?;
            Ok(DeploymentOutcome {
                action: DeploymentAction::Created,
                item_id: item.id,
            })
        }
    }
}

fn service_error(error: NotebookError) -> DeploymentError {
    DeploymentError::Service(Box::new(error))
}
