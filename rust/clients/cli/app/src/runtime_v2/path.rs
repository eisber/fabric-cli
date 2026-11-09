// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk::runtime::validate_segment;
use serde::{Deserialize, Serialize};

use super::{ReadError, ReadErrorKind};

/// The explicit artifact owner type; a facade is a constraint, not a resolver.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    /// Notebook workload content and Jupyter sessions.
    Notebook,
    /// An explicitly bound Lakehouse Livy service.
    Lakehouse,
    /// An explicitly bound native Livy service.
    LivyService,
}

/// Human-selected owner names, to be resolved by the caller.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerPath {
    /// Workspace display name, including its `.Workspace` type suffix.
    pub workspace: String,
    /// Artifact display name, including its explicit type suffix.
    pub artifact: String,
    /// Claimed type, independently checked against the resolver's typed binding.
    pub kind: OwnerKind,
}

/// An experimental, explicit read facet, not an arbitrary endpoint suffix.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadFacet {
    /// Root metadata; unsupported in this slice, never an implicit content read.
    Metadata,
    /// Full persisted NBS IPYNB including saved outputs.
    NotebookContent,
    /// Live Jupyter or classic Livy session collection, not public history.
    Sessions,
    /// An exact session identifier, never first-idle/newest selection.
    Session {
        /// Explicit Jupyter or classic Livy session ID.
        id: String,
    },
    /// Existing classic Livy statement JSON, including retained output.
    Statement {
        /// Exact classic session owner.
        session: String,
        /// Existing nonnegative signed-64-bit statement ID.
        id: u64,
        /// Explicit result view; still returns the complete native statement envelope.
        result: bool,
    },
}

/// Parsed logical path. Parsing performs no metadata, identity or endpoint discovery.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedPath {
    /// Explicit human owner.
    pub owner: OwnerPath,
    /// Selected representation or runtime resource.
    pub facet: ReadFacet,
}

impl TypedPath {
    /// Parse the bounded experimental H1 grammar documented in runtime-v2-reads.md.
    ///
    /// # Errors
    /// Rejects ambiguous/unsafe paths and unadvertised facets before any resolver call.
    pub fn parse(path: &str) -> Result<Self, ReadError> {
        let invalid = || ReadError::new(ReadErrorKind::InvalidInput);
        if path.len() > 4096 || !path.starts_with('/') {
            return Err(invalid());
        }
        let parts: Vec<_> = path[1..].split('/').collect();
        if parts.len() < 2
            || parts.iter().any(|part| {
                part.len() > 1024 || validate_segment(part).is_err() || part.trim().is_empty()
            })
        {
            return Err(invalid());
        }
        if !typed_name(parts[0], ".Workspace") {
            return Err(invalid());
        }
        let kind = if typed_name(parts[1], ".Notebook") {
            OwnerKind::Notebook
        } else if typed_name(parts[1], ".Lakehouse") {
            OwnerKind::Lakehouse
        } else if typed_name(parts[1], ".LivyService") {
            OwnerKind::LivyService
        } else {
            return Err(ReadError::new(ReadErrorKind::UnsupportedCapability));
        };
        let facet = match &parts[2..] {
            [] => ReadFacet::Metadata,
            ["src", "content"] if kind == OwnerKind::Notebook => ReadFacet::NotebookContent,
            ["sessions"] => ReadFacet::Sessions,
            ["sessions", id] if !matches!(*id, "active" | "livy-history") => {
                ReadFacet::Session { id: (*id).into() }
            }
            ["sessions", session, "statements", id]
            | ["sessions", session, "statements", id, "result"]
                if kind != OwnerKind::Notebook =>
            {
                let id = id.parse::<u64>().map_err(|_| invalid())?;
                if id > i64::MAX as u64 {
                    return Err(invalid());
                }
                ReadFacet::Statement {
                    session: (*session).into(),
                    id,
                    result: parts.len() == 7,
                }
            }
            _ => return Err(ReadError::new(ReadErrorKind::UnsupportedCapability)),
        };
        Ok(Self {
            owner: OwnerPath {
                workspace: parts[0].into(),
                artifact: parts[1].into(),
                kind,
            },
            facet,
        })
    }

    pub(super) fn validate(&self) -> Result<(), ReadError> {
        // Deserialized inputs must pass the same grammar as clap inputs.
        let suffix = match &self.facet {
            ReadFacet::Metadata => String::new(),
            ReadFacet::NotebookContent => "/src/content".into(),
            ReadFacet::Sessions => "/sessions".into(),
            ReadFacet::Session { id } => format!("/sessions/{id}"),
            ReadFacet::Statement {
                session,
                id,
                result,
            } => {
                format!(
                    "/sessions/{session}/statements/{id}{}",
                    if *result { "/result" } else { "" }
                )
            }
        };
        let parsed = Self::parse(&format!(
            "/{}/{}{}",
            self.owner.workspace, self.owner.artifact, suffix
        ))?;
        if parsed != *self {
            return Err(ReadError::new(ReadErrorKind::InvalidInput));
        }
        Ok(())
    }
}

fn typed_name(value: &str, suffix: &str) -> bool {
    value
        .strip_suffix(suffix)
        .is_some_and(|name| !name.trim().is_empty())
}
