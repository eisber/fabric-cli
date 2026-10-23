// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded literal/logical/workspace-ID transforms for a single Notebook.

use crate::notebook::DeploymentError;

/// The pinned CICD export placeholder, never a real target workspace.
pub const DEFAULT_GUID: &str = "00000000-0000-0000-0000-000000000000";

/// Explicit, already-resolved native transform inputs.
///
/// This is not a parameter.yml parser. YAML, JSONPath, regex, dynamic metadata
/// lookup and environment expansion are not accepted through this typed API.
#[derive(Clone, Debug, Default)]
pub struct NotebookTransforms {
    /// Ordered source logical-ID to deployed-ID mappings, supplied by the caller.
    pub logical_ids: Vec<(String, String)>,
    /// Ordered case-sensitive literal find/replacement pairs.
    pub literals: Vec<(String, String)>,
}

impl NotebookTransforms {
    /// Applies logical IDs, literal parameters, then default workspace IDs.
    ///
    /// # Errors
    /// Rejects empty find/logical values, referenced undeployed logical IDs, and
    /// output growth beyond the byte bound. `.platform` must bypass this helper.
    pub fn apply(
        &self,
        content: &str,
        workspace: &str,
        max_bytes: usize,
    ) -> Result<String, DeploymentError> {
        let mut content = content.to_owned();
        for (logical, deployed) in &self.logical_ids {
            if logical.is_empty() {
                return Err(DeploymentError::InvalidInput);
            }
            if logical == DEFAULT_GUID {
                continue;
            }
            if content.contains(logical) {
                if deployed.is_empty() {
                    return Err(DeploymentError::UndeployedLogicalId);
                }
                content = replace_bounded(&content, logical, deployed, max_bytes)?;
            }
        }
        for (find, replacement) in &self.literals {
            if find.is_empty() {
                return Err(DeploymentError::InvalidInput);
            }
            // Pinned find/replace skips empty resolved replacement values.
            if !replacement.is_empty() {
                content = replace_bounded(&content, find, replacement, max_bytes)?;
            }
        }
        replace_workspace_ids(&content, workspace, max_bytes)
    }
}

fn replace_bounded(
    content: &str,
    find: &str,
    replacement: &str,
    max_bytes: usize,
) -> Result<String, DeploymentError> {
    let count = content.matches(find).count();
    let length = content
        .len()
        .checked_sub(
            count
                .checked_mul(find.len())
                .ok_or(DeploymentError::Limit)?,
        )
        .and_then(|length| length.checked_add(count.checked_mul(replacement.len())?))
        .filter(|length| *length <= max_bytes)
        .ok_or(DeploymentError::Limit)?;
    let mut result = String::with_capacity(length);
    for (index, chunk) in content.split(find).enumerate() {
        if index != 0 {
            result.push_str(replacement);
        }
        result.push_str(chunk);
    }
    Ok(result)
}

fn replace_workspace_ids(
    content: &str,
    workspace: &str,
    max_bytes: usize,
) -> Result<String, DeploymentError> {
    let mut result = String::new();
    let mut position = 0;
    while position < content.len() {
        let tail = &content[position..];
        let key_start = usize::from(tail.starts_with('"'));
        let after_key = ["default_lakehouse_workspace_id", "workspaceId", "workspace"]
            .into_iter()
            .find_map(|key| tail[key_start..].strip_prefix(key));
        if let Some(after_key) = after_key {
            let after_key = after_key
                .strip_prefix('"')
                .unwrap_or(after_key)
                .trim_start();
            if let Some(after_separator) = after_key.strip_prefix([':', '='])
                && let Some(value) = after_separator.trim_start().strip_prefix('"')
                && let Some(end) = value.find('"')
            {
                let capture = &value[..end];
                if !capture.contains('\n') {
                    let consumed = tail.len() - value.len() + end + 1;
                    if capture == DEFAULT_GUID {
                        let prefix_length = tail.len() - value.len();
                        result.push_str(&tail[..prefix_length]);
                        result.push_str(workspace);
                        result.push('"');
                    } else {
                        result.push_str(&tail[..consumed]);
                    }
                    position += consumed;
                    if result.len() > max_bytes {
                        return Err(DeploymentError::Limit);
                    }
                    continue;
                }
            }
        }
        let ch = tail.chars().next().expect("nonempty tail");
        result.push(ch);
        position += ch.len_utf8();
        if result.len() > max_bytes {
            return Err(DeploymentError::Limit);
        }
    }
    Ok(result)
}
