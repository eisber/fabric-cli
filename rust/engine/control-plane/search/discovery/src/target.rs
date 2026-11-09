// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::DiscoveryError;
use serde::Serialize;

/// Local selector safety bound in UTF-8 bytes, not a Fabric resource-name limit.
pub const MAX_TARGET_BYTES: usize = 4096;

/// The only exact resource kinds in the initial discovery denominator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum TargetKind {
    /// A top-level workspace, including a prospective create destination.
    Workspace,
    /// A notebook directly beneath a typed workspace.
    Notebook,
}

impl TargetKind {
    /// Stable type vocabulary for front-end facade generation.
    ///
    /// These are lexical kinds, not resolved identities or evidence that every
    /// operation is supported. Exact operation/kind pairs use shared discovery.
    pub const ALL: [Self; 2] = [Self::Workspace, Self::Notebook];

    /// Exact, case-sensitive service type suffix.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Notebook => "Notebook",
        }
    }
}

/// An exact lexical resource selector, never a resolved service identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetSelector {
    path: String,
    kind: TargetKind,
}

impl TargetSelector {
    /// Parse `/Name.Workspace` or `/Name.Workspace/Name.Notebook`.
    ///
    /// Parsing is bounded and entirely lexical. It neither looks up existing
    /// resources nor requires the final create destination to exist. Names may
    /// contain spaces, Unicode and dots, but not embedded recognized type
    /// suffixes that would make a component ambiguous.
    ///
    /// # Errors
    /// Returns [`DiscoveryError::InvalidTarget`] for malformed selectors and
    /// [`DiscoveryError::UnsupportedCapability`] for unknown kinds or facets.
    /// In particular, `/src/definition` and `/functions/name` never fall back
    /// to their parent's contract.
    pub fn parse(path: &str) -> Result<Self, DiscoveryError> {
        if path.is_empty()
            || path.len() > MAX_TARGET_BYTES
            || !path.starts_with('/')
            || path.ends_with('/')
            || path.contains("//")
            || path.chars().any(char::is_control)
            || path.contains(['\\', ':', '%', '?', '#', '*', '[', ']', '{', '}'])
        {
            return Err(invalid("expected a bounded absolute literal typed path"));
        }
        let components: Vec<_> = path[1..].split('/').collect();
        if components
            .iter()
            .any(|part| part.is_empty() || matches!(*part, "." | "..") || part.trim() != *part)
        {
            return Err(invalid("empty, traversing or whitespace-padded component"));
        }
        if components.len() > 2 {
            return Err(unsupported(
                "resource facets and nested targets are not registered",
            ));
        }
        let first = component_kind(components[0])?;
        if first != TargetKind::Workspace {
            return Err(invalid("a resource path must begin with a typed workspace"));
        }
        let kind = if components.len() == 1 {
            first
        } else {
            let child = component_kind(components[1])?;
            if child != TargetKind::Notebook {
                return Err(unsupported(
                    "only a Notebook child of a Workspace is registered",
                ));
            }
            child
        };
        Ok(Self {
            path: path.into(),
            kind,
        })
    }

    /// Original selector, unchanged by parsing.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Lexically selected type, without existence or authorization proof.
    pub const fn kind(&self) -> TargetKind {
        self.kind
    }
}

fn component_kind(component: &str) -> Result<TargetKind, DiscoveryError> {
    let Some((name, suffix)) = component.rsplit_once('.') else {
        return Err(unsupported("an exact registered type suffix is required"));
    };
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.trim() != name
        || name
            .split('.')
            .any(|part| matches!(part, "Workspace" | "Notebook"))
    {
        return Err(invalid("empty or ambiguous typed resource name"));
    }
    match suffix {
        "Workspace" => Ok(TargetKind::Workspace),
        "Notebook" => Ok(TargetKind::Notebook),
        _ => Err(unsupported("the exact resource kind is not registered")),
    }
}

fn invalid(reason: &'static str) -> DiscoveryError {
    DiscoveryError::InvalidTarget { reason }
}

fn unsupported(reason: &'static str) -> DiscoveryError {
    DiscoveryError::UnsupportedCapability { reason }
}
