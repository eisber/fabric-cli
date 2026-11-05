// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{require_prefix, validate_scope};
use crate::runtime::{
    NativeEffect, NativeEndpoint, NativeError, NativeMethod, NativeRequest, NativeResponse,
    validate_segment,
};

/// Validated resource-relative path, never a local filesystem destination.
#[derive(Clone, Eq, PartialEq)]
pub struct ResourcePath(String);

impl ResourcePath {
    /// Validate an exact relative resource path; an empty path selects the root.
    ///
    /// # Errors
    /// Rejects absolute paths, traversal, escapes, drive/URI prefixes and controls.
    /// Unlike FNTK, surrounding slashes are rejected, not silently stripped.
    pub fn new(path: impl Into<String>) -> Result<Self, NativeError> {
        let path = path.into();
        if path.contains(':') {
            return Err(NativeError::InvalidInput);
        }
        if !path.is_empty() {
            for segment in path.split('/') {
                validate_segment(segment)?;
            }
        }
        Ok(Self(path))
    }

    /// Exact provider-relative path, not normalized.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn segments(&self) -> Vec<&str> {
        if self.0.is_empty() { Vec::new() } else { self.0.split('/').collect() }
    }

    fn require_entry(&self) -> Result<(), NativeError> {
        if self.0.is_empty() { return Err(NativeError::InvalidInput); }
        Ok(())
    }
}

impl std::fmt::Debug for ResourcePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResourcePath(<relative>)")
    }
}

/// Provider filesystem entry type, sent as `Ms-FileSystem-Entry-Type`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryType {
    /// File.
    File,
    /// Folder.
    Folder,
}

impl EntryType {
    const fn as_str(self) -> &'static str {
        match self { Self::File => "file", Self::Folder => "folder" }
    }
}

/// Explicit overwrite admission, not a local existence-check race.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceWriteAdmission {
    /// Native PUT may replace an existing entry or create an absent one.
    Upsert,
    /// Unsupported: the pinned implementation sends no enforced precondition.
    CreateOnly,
}

impl ResourceWriteAdmission {
    fn validate(self) -> Result<(), NativeError> {
        match self {
            Self::Upsert => Ok(()),
            Self::CreateOnly => Err(NativeError::UnsupportedCapability),
        }
    }
}

/// Downloaded content held only in caller-owned memory; Debug is redacted.
pub enum ResourceContent {
    /// UTF-8 text according to text/json/xml response media.
    Text(String),
    /// Exact binary bytes for all other or absent media types.
    Binary(Vec<u8>),
}

impl ResourceContent {
    /// Decode an already bounded native response without writing a local file.
    ///
    /// # Errors
    /// Preserves non-success HTTP statuses and rejects malformed UTF-8 text.
    pub fn from_response(response: NativeResponse) -> Result<Self, NativeError> {
        if !(200..300).contains(&response.status) { return Err(NativeError::Http(response.status)); }
        let media = response.content_type.as_deref().unwrap_or("").to_ascii_lowercase();
        if media.contains("text") || media.contains("json") || media.contains("xml") {
            Ok(Self::Text(String::from_utf8(response.body).map_err(|_| NativeError::InvalidResponse)?))
        } else {
            Ok(Self::Binary(response.body))
        }
    }
}

impl std::fmt::Debug for ResourceContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResourceContent(<redacted>)")
    }
}

/// Typed resource filesystem operations bound to an exact caller-supplied base.
#[derive(Clone, Debug)]
pub struct Resources {
    endpoint: NativeEndpoint,
}

impl Resources {
    /// Bind the native `Data/Direct/.../filesystem` prefix, not a SparkUI base.
    ///
    /// # Errors
    /// Rejects unsafe IDs and mismatched capability/owner prefixes.
    pub fn new(endpoint: NativeEndpoint, capacity: &str, workspace: &str, artifact: &str) -> Result<Self, NativeError> {
        validate_scope(&[capacity, workspace, artifact])?;
        require_prefix(&endpoint, &format!("/webapi/capacities/{capacity}/workloads/Notebook/Data/Direct/api/workspaces/{workspace}/artifacts/{artifact}/filesystem"))?;
        Ok(Self { endpoint })
    }

    fn request(&self, path: &ResourcePath, method: NativeMethod, query: &[(&str, String)], effect: NativeEffect) -> Result<NativeRequest, NativeError> {
        let mut segments = vec!["workdir"];
        segments.extend(path.segments());
        self.endpoint.request(method, &segments, query, None, effect)
    }

    /// List file and folder entries, explicitly selecting recursion.
    ///
    /// The provider JSON is preserved; no undocumented listing schema is invented.
    ///
    /// # Errors
    /// Fails for invalid scoped route segments before dispatch.
    pub fn list(&self, path: &ResourcePath, recursive: bool) -> Result<NativeRequest, NativeError> {
        self.request(path, NativeMethod::Get, &[("recursive", recursive.to_string())], NativeEffect::Read)?.with_header("Cache-Control", "no-cache")
    }

    /// Read exact file content as bytes; response decoding is explicit.
    ///
    /// # Errors
    /// Rejects a root-as-file request.
    pub fn get(&self, path: &ResourcePath) -> Result<NativeRequest, NativeError> {
        path.require_entry()?;
        self.request(path, NativeMethod::Get, &[], NativeEffect::Read)?.with_accept("*/*")
    }

    /// Upload exact bytes with explicit media, bound and upsert admission.
    ///
    /// No `If-None-Match` is manufactured from the misleading source docstring.
    ///
    /// # Errors
    /// Rejects create-only admission, root, invalid media and oversized payloads.
    pub fn update(&self, path: &ResourcePath, bytes: Vec<u8>, media: &str, max_request_bytes: usize, admission: ResourceWriteAdmission) -> Result<NativeRequest, NativeError> {
        admission.validate()?;
        path.require_entry()?;
        self.request(path, NativeMethod::Put, &[], NativeEffect::Update)?
            .with_bytes(bytes, media, max_request_bytes)?
            .with_header("Ms-FileSystem-Entry-Type", "file")
    }

    /// Ensure a native folder with the same explicit upsert admission.
    ///
    /// The source sends a bodyless PUT with shared `application/json` media.
    ///
    /// # Errors
    /// Rejects root and unsupported create-only admission before dispatch.
    pub fn update_folder(&self, path: &ResourcePath, admission: ResourceWriteAdmission) -> Result<NativeRequest, NativeError> {
        admission.validate()?;
        path.require_entry()?;
        self.request(path, NativeMethod::Put, &[], NativeEffect::Update)?.with_header("Ms-FileSystem-Entry-Type", "folder")
    }

    /// Delete an exact entry, declaring type and recursion explicitly.
    ///
    /// # Errors
    /// Rejects root deletion and meaningless recursive file deletion.
    pub fn delete(&self, path: &ResourcePath, entry: EntryType, recursive: bool) -> Result<NativeRequest, NativeError> {
        path.require_entry()?;
        if entry == EntryType::File && recursive { return Err(NativeError::InvalidInput); }
        let query = if recursive { vec![("recursive", "true".into())] } else { Vec::new() };
        self.request(path, NativeMethod::Delete, &query, NativeEffect::Delete)?.with_header("Ms-FileSystem-Entry-Type", entry.as_str())
    }

    /// Read actual provider usage/quota JSON, not a synthesized local estimate.
    ///
    /// # Errors
    /// Fails for invalid scoped route segments before dispatch.
    pub fn usage(&self) -> Result<NativeRequest, NativeError> {
        self.endpoint.request(NativeMethod::Get, &["workdirUsage"], &[], None, NativeEffect::Read)
    }
}
