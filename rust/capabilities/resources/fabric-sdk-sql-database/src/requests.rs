// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    FolderId, SqlDatabase, SqlDatabasePage, SqlDatabaseTarget, SqlInputError, SqlManagementError,
    SqlResponseError, SqlResponseErrorKind, WorkspaceId,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceMethod, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::Value;
use std::fmt;

/// A caller-supplied nonempty opaque token, encoded once as a query value.
#[derive(Clone, Eq, PartialEq)]
pub struct ContinuationToken(String);

impl ContinuationToken {
    /// Validates opaque token syntax without interpreting or pre-encoding it.
    ///
    /// # Errors
    /// Rejects empty/control-bearing tokens before descriptor or credential work.
    pub fn new(value: impl Into<String>) -> Result<Self, SqlInputError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(SqlInputError::InvalidContinuationToken);
        }
        Ok(Self(value))
    }

    /// Borrows the exact decoded service token for explicit authorized use.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ContinuationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContinuationToken(<redacted>)")
    }
}

/// Explicit folder selection for the native SQLDatabase LIST route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FolderScope {
    /// Use the workspace root by omitting rootFolderId.
    Workspace,
    /// Send the concrete folder ID.
    Folder(FolderId),
}

/// Explicit service options; no CLI recursion, folder or pagination defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListSqlDatabases {
    /// Required caller choice, always sent as true or false.
    pub recursive: bool,
    /// Required caller choice between workspace root and a concrete folder.
    pub root_folder: FolderScope,
    /// Actual decoded token from the previous page, or explicit absence.
    pub continuation_token: Option<ContinuationToken>,
}

/// Effect facts for this metadata-only SDK leaf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementEffect {
    /// Read item metadata without source retrieval or SQL submission.
    ReadMetadata,
}

/// Documented resource permission, not local proof of authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataPermission {
    /// Item Read permission for GET.
    ItemRead,
    /// Viewer-or-higher workspace role for LIST.
    WorkspaceViewer,
}

/// Documented delegated permission alternatives, not token acquisition scopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelegatedScopeAlternatives {
    /// SQLDatabase.Read.All/ReadWrite.All or Item.Read.All/ReadWrite.All.
    SqlDatabaseOrItemReadOrReadWrite,
    /// Workspace.Read.All or Workspace.ReadWrite.All.
    WorkspaceReadOrReadWrite,
}

/// Read-only operation facts consumed by an existing caller/host policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SqlOperationSemantics {
    /// Documented effect; no approval protocol is installed.
    pub effect: ManagementEffect,
    /// Required resource permission, enforced authoritatively by the service.
    pub permission: MetadataPermission,
    /// Delegated permission alternatives for this native endpoint.
    pub scopes: DelegatedScopeAlternatives,
}

struct ExpectedIdentity {
    workspace: WorkspaceId,
    database: Option<crate::SqlDatabaseId>,
}

/// A fixed typed metadata request plus decoder, not a transport or poller.
///
/// Descriptors always use ReturnAccepted as a non-waiting shared policy. Only
/// completed HTTP 200 is a valid result; unexpected acceptance is not success.
pub struct SqlRequest<T> {
    operation: ResourceOperation,
    expected: ExpectedIdentity,
    decoder: fn(Value, &ExpectedIdentity) -> Result<T, SqlResponseErrorKind>,
    semantics: SqlOperationSemantics,
}

impl<T> fmt::Debug for SqlRequest<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlRequest")
            .field("operation", &self.operation)
            .field("semantics", &self.semantics)
            .finish()
    }
}

impl<T> SqlRequest<T> {
    /// Borrows the validated fixed-route descriptor for shared execution.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }

    /// Returns documented effects/permissions without checking actual rights.
    pub const fn semantics(&self) -> &SqlOperationSemantics {
        &self.semantics
    }

    /// Decodes one shared outcome without fetching, retrying or projecting paths.
    ///
    /// # Errors
    /// Rejects acceptance, undocumented statuses, malformed known fields and
    /// identities different from the selected workspace/item.
    pub fn decode(&self, outcome: ResourceOutcome) -> Result<T, SqlManagementError> {
        let (status, data) = match outcome {
            ResourceOutcome::Accepted { operation_id } => {
                return Err(SqlManagementError::Response(SqlResponseError {
                    kind: SqlResponseErrorKind::UnexpectedAcceptance,
                    http_status: 202,
                    operation_id,
                }));
            }
            ResourceOutcome::Completed { status, data } => (status, data),
        };
        let reject = |kind| {
            SqlManagementError::Response(SqlResponseError {
                kind,
                http_status: status,
                operation_id: None,
            })
        };
        if status != 200 {
            return Err(reject(SqlResponseErrorKind::UnexpectedStatus));
        }
        let data = data.ok_or_else(|| reject(SqlResponseErrorKind::MissingBody))?;
        (self.decoder)(data, &self.expected).map_err(reject)
    }

    /// Delegates exactly once to the caller's existing shared operation policy.
    ///
    /// # Errors
    /// Preserves the original credential/execution/service error as data. No
    /// fallback, acceptance polling, mutation or automatic page request is added.
    pub async fn execute(
        self,
        executor: &mut dyn ResourceOperations,
    ) -> Result<T, SqlManagementError> {
        let outcome = executor
            .execute(self.operation.clone())
            .await
            .map_err(SqlManagementError::Operation)?;
        self.decode(outcome)
    }
}

/// Native metadata descriptors; no mutation or unsupported endpoint stubs.
pub struct SqlDatabaseRequests;

impl SqlDatabaseRequests {
    /// Prepares GET on the concrete SQLDatabase collection, with no augmentation.
    ///
    /// # Errors
    /// Returns a sanitized descriptor failure before any execution.
    pub fn get(target: &SqlDatabaseTarget) -> Result<SqlRequest<SqlDatabase>, SqlInputError> {
        let workspace = target.workspace_id().to_string();
        let database = target.database_id().to_string();
        let operation = ResourceOperation::new(
            ResourceMethod::Get,
            &["workspaces", &workspace, "sqlDatabases", &database],
            None,
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(|_| SqlInputError::InvalidDescriptor)?;
        Ok(SqlRequest {
            operation,
            expected: ExpectedIdentity {
                workspace: target.workspace_id(),
                database: Some(target.database_id()),
            },
            decoder: decode_database,
            semantics: SqlOperationSemantics {
                effect: ManagementEffect::ReadMetadata,
                permission: MetadataPermission::ItemRead,
                scopes: DelegatedScopeAlternatives::SqlDatabaseOrItemReadOrReadWrite,
            },
        })
    }

    /// Prepares one native SQLDatabase LIST page using explicit caller options.
    ///
    /// # Errors
    /// Returns a sanitized descriptor failure before any execution. The caller
    /// must handle any returned continuation; this function never traverses it.
    pub fn list_page(
        workspace: &WorkspaceId,
        options: &ListSqlDatabases,
    ) -> Result<SqlRequest<SqlDatabasePage>, SqlInputError> {
        let workspace_text = workspace.to_string();
        let recursive = if options.recursive { "true" } else { "false" };
        let mut query = vec![("recursive", recursive.to_owned())];
        if let FolderScope::Folder(folder) = &options.root_folder {
            query.push(("rootFolderId", folder.to_string()));
        }
        if let Some(token) = &options.continuation_token {
            query.push(("continuationToken", token.as_str().to_owned()));
        }
        let query_refs: Vec<_> = query
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let operation = ResourceOperation::new(
            ResourceMethod::Get,
            &["workspaces", &workspace_text, "sqlDatabases"],
            None,
            CompletionPolicy::ReturnAccepted,
        )
        .and_then(|operation| operation.with_query(&query_refs))
        .map_err(|_| SqlInputError::InvalidDescriptor)?;
        Ok(SqlRequest {
            operation,
            expected: ExpectedIdentity {
                workspace: *workspace,
                database: None,
            },
            decoder: decode_page,
            semantics: SqlOperationSemantics {
                effect: ManagementEffect::ReadMetadata,
                permission: MetadataPermission::WorkspaceViewer,
                scopes: DelegatedScopeAlternatives::WorkspaceReadOrReadWrite,
            },
        })
    }
}

fn check_identity(
    database: &SqlDatabase,
    expected: &ExpectedIdentity,
) -> Result<(), SqlResponseErrorKind> {
    if database.workspace_id != expected.workspace
        || expected
            .database
            .is_some_and(|expected| database.id != expected)
    {
        return Err(SqlResponseErrorKind::IdentityMismatch);
    }
    Ok(())
}

fn decode_database(
    data: Value,
    expected: &ExpectedIdentity,
) -> Result<SqlDatabase, SqlResponseErrorKind> {
    let database = serde_json::from_value(data).map_err(|_| SqlResponseErrorKind::InvalidShape)?;
    check_identity(&database, expected)?;
    Ok(database)
}

fn decode_page(
    data: Value,
    expected: &ExpectedIdentity,
) -> Result<SqlDatabasePage, SqlResponseErrorKind> {
    let page: SqlDatabasePage =
        serde_json::from_value(data).map_err(|_| SqlResponseErrorKind::InvalidShape)?;
    for database in &page.value {
        check_identity(database, expected)?;
    }
    Ok(page)
}
