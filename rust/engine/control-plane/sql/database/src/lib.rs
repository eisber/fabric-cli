// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed SQLDatabase metadata GET and single-page LIST over shared operations.
//!
//! Requests contain no credentials or runtime. Inject the existing shared
//! operation session; no endpoint-specific HTTP, paging or polling engine exists.
//! Results are a forward-compatible read-only projection, not writable metadata.

mod errors;
mod ids;
mod models;
mod requests;

/// Warehouse-owned management metadata, connections and restore-point reads.
pub mod warehouse;

pub use errors::{SqlInputError, SqlManagementError, SqlResponseError, SqlResponseErrorKind};
pub use ids::{FolderId, Guid, SqlDatabaseId, SqlDatabaseTarget, WorkspaceId};
pub use models::{
    ConnectionStringMetadata, Observed, PageCoverage, SqlDatabase, SqlDatabasePage,
    SqlDatabaseProperties,
};
pub use requests::{
    ContinuationToken, DelegatedScopeAlternatives, FolderScope, ListSqlDatabases, ManagementEffect,
    MetadataPermission, SqlDatabaseRequests, SqlOperationSemantics, SqlRequest,
};
