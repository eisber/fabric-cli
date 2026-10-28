// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Stable, bounded deployment snapshots, distinct from definition-only import.

use crate::deployment_config::{ConfigurationError, ITEM_TYPES};
use serde_json::Value;
use std::fmt;

/// One repository definition part, with original binary bytes retained.
#[derive(Clone)]
pub struct ItemFile {
    /// Validated portable item-relative path.
    pub path: String,
    /// Exact bytes (universal newlines normalized only for text).
    pub bytes: Vec<u8>,
    /// Source file classification; binary content bypasses text processors.
    pub text: bool,
}

impl fmt::Debug for ItemFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemFile")
            .field("path", &self.path)
            .field("bytes", &self.bytes.len())
            .field("text", &self.text)
            .finish()
    }
}

impl ItemFile {
    /// Load a file without interpreting its definition schema.
    pub fn new(path: String, bytes: Vec<u8>) -> Self {
        let binary = bytes.contains(&0)
            || std::str::from_utf8(&bytes).is_err()
            || [
                b"%PDF".as_slice(),
                b"GIF87a",
                b"GIF89a",
                b"BM",
                b"PK\x03\x04",
                b"PK\x05\x06",
                b"PK\x07\x08",
                b"BZh",
                b"MZ",
                b"OTTO",
                b"wOFF",
                b"wOF2",
            ]
            .iter()
            .any(|magic| bytes.starts_with(magic))
            || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
            || bytes.get(257..262) == Some(b"ustar");
        let bytes = if binary {
            bytes
        } else {
            String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .into_bytes()
        };
        Self {
            path,
            bytes,
            text: !binary,
        }
    }

    /// Last path component, independent of the host operating system.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Read classified text without lossy binary conversion.
    ///
    /// # Errors
    /// Binary content is not a valid input for a text-only processor.
    pub fn content(&self) -> Result<&str, ConfigurationError> {
        std::str::from_utf8(&self.bytes).map_err(|_| invalid("Expected a text definition file"))
    }
}

/// One source item, with immutable metadata and a command-local deployed identity.
#[derive(Clone, Debug)]
pub struct RepositoryItem {
    /// Exact metadata display name.
    pub name: String,
    /// Exact Python-supported type.
    pub item_type: String,
    /// Source description, including the empty default.
    pub description: String,
    /// Source `.platform` logical ID.
    pub logical_id: String,
    /// Portable repository-relative item directory.
    pub path: String,
    /// Workspace folder path; empty is the root.
    pub folder_path: String,
    /// Complete ordered input snapshot, including `.platform`.
    pub files: Vec<ItemFile>,
}

impl RepositoryItem {
    /// Decode actual `.platform` metadata, never infer a type from a directory suffix.
    ///
    /// # Errors
    /// Missing metadata, invalid types, and empty logical IDs fail local preflight.
    pub fn from_files(path: String, files: Vec<ItemFile>) -> Result<Self, ConfigurationError> {
        let platform = files
            .iter()
            .find(|file| file.path == ".platform")
            .ok_or_else(|| invalid("Missing .platform metadata"))?;
        let platform: Value = serde_json::from_slice(&platform.bytes)
            .map_err(|_| invalid("Invalid .platform JSON"))?;
        let metadata = platform
            .get("metadata")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("Missing .platform metadata dictionary"))?;
        let required = |value: Option<&Value>, field: &str| {
            value
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| invalid(field))
        };
        let name = required(metadata.get("displayName"), "Missing displayName")?;
        let item_type = required(metadata.get("type"), "Missing item type")?;
        if !ITEM_TYPES.contains(&item_type.as_str()) {
            return Err(invalid(
                "Item type is not supported by the pinned Python source",
            ));
        }
        let logical_id = required(
            platform.pointer("/config/logicalId"),
            "logicalId cannot be empty",
        )?;
        let description = metadata
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into();
        let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let folder_path = if matches!(item_type.as_str(), "KQLDatabase" | "OrgAppAudience") {
            parent
                .split_once("/.children")
                .map_or(parent, |(container, _)| {
                    container.rsplit_once('/').map_or("", |(parent, _)| parent)
                })
        } else {
            parent
        };
        let folder_path = if folder_path.is_empty() {
            String::new()
        } else {
            format!("/{folder_path}")
        };
        Ok(Self {
            name,
            item_type,
            description,
            logical_id,
            path,
            folder_path,
            files,
        })
    }

    /// Exact file lookup, not a suffix or substring match.
    pub fn file(&self, path: &str) -> Option<&ItemFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

pub(crate) fn invalid(message: &str) -> ConfigurationError {
    ConfigurationError::Invalid(vec![message.into()])
}
