// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed retrieval paths; parsing never performs discovery or authentication.

/// One selected retrieval representation, distinct from output serialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetRepresentation {
    /// Primary resource metadata.
    Metadata,
    /// Native multipart definition or an exact advertised part.
    Definition {
        /// Explicit service-native selector, not JSON/Markdown output format.
        native_format: Option<String>,
        /// Exact native part path, never a local extraction destination.
        part: Option<String>,
    },
    /// An exact file below the item's Files root.
    File(String),
    /// Read-oriented view below docs, resolved by an advertised provider.
    Docs(String),
    /// A returned item-job handle.
    Job {
        /// Exact provider-returned job identifier.
        job_id: String,
    },
}

/// One typed resource selector, before bounded name resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetTarget {
    /// A workspace display name or explicit GUID with the Workspace suffix.
    Workspace {
        /// Exact workspace selector.
        workspace: String,
    },
    /// A typed item in an explicit workspace.
    Item {
        /// Exact workspace selector.
        workspace: String,
        /// Exact item selector.
        item: String,
        /// Declared Fabric type; `None` for an explicit ID-only machine path.
        item_type: Option<String>,
        /// Selected representation.
        representation: GetRepresentation,
    },
    /// An already-returned Fabric operation, not a submitting request.
    Operation {
        /// Exact operation identifier.
        operation_id: String,
        /// Whether its explicit result child was addressed.
        result: bool,
    },
}

/// Invalid or unsupported paths never become guessed metadata reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GetPathError {
    /// The spelling is not an explicit absolute typed target.
    #[error("get requires an absolute typed resource or returned-handle path")]
    InvalidPath,
    /// The addressed representation has no advertised binding in this head.
    #[error("the addressed retrieval representation is not supported")]
    UnsupportedRepresentation,
}

impl GetTarget {
    /// Parse the supported typed and returned-handle path grammar.
    ///
    /// Name selectors use `/Name.Workspace/Item.Type`. Explicit machine IDs may
    /// use `/workspaces/{id}/items/{id}`. Operation receipts use
    /// `/operations/{id}`; runs are item-scoped. No URL, dot traversal, percent
    /// escaping or native suffix normalization is performed.
    ///
    /// # Errors
    /// Rejects invalid paths and unsupported facets instead of guessing a read.
    pub fn parse(path: &str) -> Result<Self, GetPathError> {
        if !path.starts_with('/')
            || path.ends_with('/')
            || path.contains(['\\', '?', '#', '%', '\0'])
            || path.chars().any(char::is_control)
        {
            return Err(GetPathError::InvalidPath);
        }
        let segments: Vec<_> = path[1..].split('/').collect();
        if segments
            .iter()
            .any(|value| value.is_empty() || matches!(*value, "." | ".."))
        {
            return Err(GetPathError::InvalidPath);
        }
        if segments.first() == Some(&"operations") {
            return match segments.as_slice() {
                [_, id] if is_id(id) => Ok(Self::Operation {
                    operation_id: (*id).into(),
                    result: false,
                }),
                [_, id, "result"] if is_id(id) => Ok(Self::Operation {
                    operation_id: (*id).into(),
                    result: true,
                }),
                _ => Err(GetPathError::InvalidPath),
            };
        }
        if segments.first() == Some(&"workspaces") {
            return match segments.as_slice() {
                [_, id] if is_id(id) => Ok(Self::Workspace {
                    workspace: (*id).into(),
                }),
                [_, workspace, "items", item, rest @ ..] if is_id(workspace) && is_id(item) => {
                    Ok(Self::Item {
                        workspace: (*workspace).into(),
                        item: (*item).into(),
                        item_type: None,
                        representation: representation(rest)?,
                    })
                }
                _ => Err(GetPathError::InvalidPath),
            };
        }
        let workspace = segments[0]
            .strip_suffix(".Workspace")
            .filter(|value| !value.is_empty())
            .ok_or(GetPathError::InvalidPath)?;
        if segments.len() == 1 {
            return Ok(Self::Workspace {
                workspace: workspace.into(),
            });
        }
        let (item, kind) = segments[1]
            .rsplit_once('.')
            .filter(|(item, kind)| {
                !item.is_empty()
                    && !kind.is_empty()
                    && kind.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .ok_or(GetPathError::InvalidPath)?;
        Ok(Self::Item {
            workspace: workspace.into(),
            item: item.into(),
            item_type: Some(kind.into()),
            representation: representation(&segments[2..])?,
        })
    }
}

fn representation(parts: &[&str]) -> Result<GetRepresentation, GetPathError> {
    match parts {
        [] => Ok(GetRepresentation::Metadata),
        ["docs", rest @ ..] => Ok(GetRepresentation::Docs(rest.join("/"))),
        ["Files", rest @ ..] if !rest.is_empty() => {
            Ok(GetRepresentation::File(format!("Files/{}", rest.join("/"))))
        }
        ["src", "definition"] => Ok(GetRepresentation::Definition {
            native_format: None,
            part: None,
        }),
        ["src", "definition", "parts", rest @ ..] if !rest.is_empty() => {
            Ok(GetRepresentation::Definition {
                native_format: None,
                part: Some(rest.join("/")),
            })
        }
        ["src", "definition", "formats", format] => Ok(GetRepresentation::Definition {
            native_format: Some((*format).into()),
            part: None,
        }),
        ["src", "definition", "formats", format, "parts", rest @ ..] if !rest.is_empty() => {
            Ok(GetRepresentation::Definition {
                native_format: Some((*format).into()),
                part: Some(rest.join("/")),
            })
        }
        ["runs", id] if is_id(id) => Ok(GetRepresentation::Job {
            job_id: (*id).into(),
        }),
        _ => Err(GetPathError::UnsupportedRepresentation),
    }
}

/// Whether a selector is a canonical GUID-shaped explicit provider identifier.
pub fn is_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
