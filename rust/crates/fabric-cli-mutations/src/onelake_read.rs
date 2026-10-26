// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{MutationError, create::truthy, message};
use fabric_cli_output::OutputInput;
use fabric_cli_query::QueryError;
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutor, OneLakeLimits, OneLakePath,
};
use serde_json::Value;
use std::{fmt, future::Future};

/// Typed OneLake read-command failures; lookup failures never imply absence.
#[derive(Clone, Debug, PartialEq)]
pub enum OneLakeReadError {
    /// Focused SDK path, selection, transport, service or traversal failure.
    Sdk(OneLakeError),
    /// Existing CLI query evaluation failed.
    Query(QueryError),
}

impl OneLakeReadError {
    /// Convert to the existing output boundary without losing the typed source.
    ///
    /// Other storage body-specific diagnostics are not invented when the SDK has
    /// retained only a status/header code. The caller can inspect this error.
    pub fn output(&self) -> OutputInput {
        let error = match self {
            Self::Query(error) => MutationError::new(error.code(), error.message()),
            Self::Sdk(OneLakeError::Http { status: 401, code }) => MutationError::new(
                code.as_deref()
                    .filter(|code| !code.is_empty())
                    .unwrap_or("Unauthorized"),
                "Access is unauthorized",
            ),
            Self::Sdk(OneLakeError::Http { status: 403, code }) => MutationError::new(
                code.as_deref()
                    .filter(|code| !code.is_empty())
                    .unwrap_or("Forbidden"),
                "Access is forbidden. You do not have permission to access this resource",
            ),
            Self::Sdk(OneLakeError::Http { status: 404, .. }) => {
                MutationError::new("NotFound", "The requested resource could not be found")
            }
            Self::Sdk(error) => MutationError::new(
                match error {
                    OneLakeError::InvalidInput => "InvalidInput",
                    OneLakeError::InvalidAudience => "InvalidAudience",
                    OneLakeError::Credential => "Credential",
                    OneLakeError::Execution(_) => "ExecutionFailed",
                    OneLakeError::Http { code, .. } => code
                        .as_deref()
                        .filter(|code| !code.is_empty())
                        .unwrap_or("UnknownError"),
                    OneLakeError::InvalidResponse => "InvalidResponse",
                    OneLakeError::Limit => "Limit",
                    OneLakeError::RepeatedContinuation => "RepeatedContinuation",
                    OneLakeError::Cancelled => "LongRunningOperationCancelled",
                },
                error.to_string(),
            ),
        };
        error.output()
    }
}

impl fmt::Display for OneLakeReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sdk(error) => error.fmt(formatter),
            Self::Query(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for OneLakeReadError {}

impl From<OneLakeError> for OneLakeReadError {
    fn from(error: OneLakeError) -> Self {
        Self::Sdk(error)
    }
}
impl From<QueryError> for OneLakeReadError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

/// OneLake read output for the existing caller-owned renderer.
#[derive(Clone, Debug)]
pub struct OneLakeReadResult {
    /// Projected data or the string exists message, using the existing envelope.
    pub output: OutputInput,
    /// Python's `show_headers` option for the text-mode host.
    pub show_headers: bool,
}

/// Original metadata and source-projected GET output for a caller-owned renderer.
#[derive(Clone, Debug)]
pub struct OneLakeGetResult {
    /// Actual source-compatible metadata, available for separately owned export.
    pub metadata: Value,
    /// Actual projection, including falsey values for a separately owned exporter.
    pub projected: Value,
    /// Output is absent when the source query returns a falsey result.
    pub output: Option<OutputInput>,
}

/// Thin OneLake `ls`/`exists` composition, not a new parser or command namespace.
///
/// A caller can pass `fabric_sdk_onelake_http::OneLakeHttpExecutor` from its own
/// dependency graph as `executor`; this crate does not select a transport.
pub struct OneLakeReadHost<'a> {
    /// Focused SDK with explicitly injected storage credential/selection.
    pub client: &'a OneLakeClient,
    /// Existing shared HTTP adapter or a synthetic offline executor.
    pub executor: &'a dyn OneLakeExecutor,
    /// Finite caller-selected traversal/response bounds.
    pub limits: OneLakeLimits,
}

impl OneLakeReadHost<'_> {
    /// Compose the source item's root-folder LS branch with exact route/columns.
    ///
    /// Root names use the source's second slash-separated component, not the
    /// directory-child name/type logic. Queries still run before column filtering.
    ///
    /// # Errors
    /// Preserves explicit SDK/query/metadata bounds; context command support is
    /// selected by the existing main hierarchy host, not rediscovered here.
    pub async fn list_item_roots(
        &self,
        workspace: &str,
        item: &str,
        long: bool,
        query: Option<&str>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeReadResult, OneLakeReadError> {
        let metadata = self
            .client
            .item_roots(self.executor, workspace, item, self.limits, cancelled)
            .await?
            .data;
        let mut rows = metadata
            .get("paths")
            .and_then(Value::as_array)
            .ok_or(OneLakeError::InvalidResponse)?
            .clone();
        for row in &mut rows {
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .ok_or(OneLakeError::InvalidResponse)?;
            let name = if name.contains('/') {
                name.split('/').nth(1).unwrap_or("")
            } else {
                name
            };
            row["name"] = name.to_owned().into();
        }
        let columns: &[&str] = if long {
            &["permissions", "lastModified", "name"]
        } else {
            &["name"]
        };
        Ok(OneLakeReadResult {
            output: OutputInput {
                data: Some(project_rows(rows, columns, query)?),
                ..OutputInput::default()
            },
            show_headers: long,
        })
    }

    /// Compose the OneLake directory-metadata GET branch and existing query layer.
    ///
    /// Unlike LS/EXISTS, source GET retains the supplied local path rather than
    /// removing Shortcut literals. It removes only `ContinuationToken` (capital C)
    /// before projection. Missing/empty query inventories JSON paths at the
    /// caller's depth, and falsey projections emit no output, as in Python.
    ///
    /// # Errors
    /// Preserves SDK/query failures and bounds; filesystem output/export and
    /// shortcut-specific metadata GET are separately owned composition branches.
    pub async fn get(
        &self,
        path: &OneLakePath,
        query: Option<&str>,
        deep: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeGetResult, OneLakeReadError> {
        let mut metadata = self
            .client
            .directory_metadata(self.executor, path, self.limits, cancelled)
            .await?
            .data;
        metadata
            .as_object_mut()
            .ok_or(OneLakeError::InvalidResponse)?
            .remove("ContinuationToken");
        let data = fabric_cli_query::search_with_options(
            &metadata,
            query.unwrap_or(""),
            fabric_cli_query::SearchOptions {
                deep_traversal: deep,
            },
        )?;
        let output = truthy(&data).then(|| OutputInput {
            data: Some(data.clone()),
            ..OutputInput::default()
        });
        Ok(OneLakeGetResult {
            metadata,
            projected: data,
            output,
        })
    }

    /// List an item-relative OneLake directory, preserving source row/query rules.
    ///
    /// `.Shortcut` and `.shortcut` are removed everywhere in the local path, as
    /// pinned Python does; other casing is not normalized. Truthy string values
    /// such as `"false"` still select the source directory/shortcut branches.
    ///
    /// # Errors
    /// Unsafe paths, malformed/incomplete pages, service/credential failures,
    /// cancellation and invalid queries remain explicit, not empty success.
    pub async fn list(
        &self,
        path: &OneLakePath,
        long: bool,
        query: Option<&str>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeReadResult, OneLakeReadError> {
        let path = source_path(path)?;
        let metadata = self
            .client
            .directory_metadata(self.executor, &path, self.limits, cancelled)
            .await?
            .data;
        let mut rows = metadata
            .get("paths")
            .and_then(Value::as_array)
            .ok_or(OneLakeError::InvalidResponse)?
            .clone();
        for row in &mut rows {
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .ok_or(OneLakeError::InvalidResponse)?;
            let separator = format!("/{}", path.relative());
            let original = name
                .rsplit_once(&separator)
                .map_or(name, |(_, name)| name)
                .trim_start_matches('/');
            let (name, kind) = if row.get("isShortcut").is_some_and(truthy) {
                (format!("{original}.Shortcut"), "Shortcut")
            } else if row.get("isDirectory").is_some_and(truthy) {
                (original.to_owned(), "Directory")
            } else {
                (original.to_owned(), "File")
            };
            row["name"] = name.into();
            row["type"] = kind.into();
        }
        let columns: &[&str] = if long {
            &["permissions", "lastModified", "name", "type"]
        } else {
            &["name"]
        };
        let data = project_rows(rows, columns, query)?;
        Ok(OneLakeReadResult {
            output: OutputInput {
                data: Some(data),
                ..OutputInput::default()
            },
            show_headers: long,
        })
    }

    /// Execute OneLake `exists` using the source's directory-list request.
    ///
    /// # Errors
    /// Only HTTP 404 becomes the string `false`; forbidden/credential/transport/
    /// malformed/traversal failures remain errors. A complete empty list is true.
    pub async fn exists(
        &self,
        path: &OneLakePath,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeReadResult, OneLakeReadError> {
        let path = source_path(path)?;
        let exists = match self
            .client
            .list(self.executor, &path, false, self.limits, cancelled)
            .await
        {
            Ok(_) => true,
            Err(OneLakeError::Http { status: 404, .. }) => false,
            Err(error) => return Err(error.into()),
        };
        Ok(OneLakeReadResult {
            output: message(if exists { "true" } else { "false" }),
            show_headers: false,
        })
    }
}

fn project_rows(
    rows: Vec<Value>,
    columns: &[&str],
    query: Option<&str>,
) -> Result<Value, QueryError> {
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        return fabric_cli_query::search(&Value::Array(rows), query);
    }
    Ok(Value::Array(
        rows.into_iter()
            .map(|row| {
                Value::Object(
                    columns
                        .iter()
                        .filter_map(|key| {
                            row.get(*key)
                                .cloned()
                                .map(|value| ((*key).to_owned(), value))
                        })
                        .collect(),
                )
            })
            .collect(),
    ))
}

fn source_path(path: &OneLakePath) -> Result<OneLakePath, OneLakeError> {
    OneLakePath::new(
        path.workspace(),
        path.item(),
        &path
            .relative()
            .replace(".Shortcut", "")
            .replace(".shortcut", ""),
    )
}
