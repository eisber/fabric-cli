// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Lakehouse-owned subresources; no independent table artifact or generic loader.

/// Table schema, existing-file loading and native Delta maintenance.
pub mod tables {
    pub use crate::capabilities::TableOperation;
    pub use crate::lakehouse::*;
    pub use crate::maintenance_jobs as jobs;
    pub use crate::{
        LoadMode, LoadOutcome, SourceKind, TableClient, TableError, TableErrorKind, TableExecutor,
        TableFuture, TableLimits, TablePath, TableRequest, TableResponse,
    };

    /// Describe a Lakehouse table operation without configuring service execution.
    pub fn describe(operation: TableOperation) -> serde_json::Value {
        crate::capabilities::describe_json(
            crate::capabilities::ParentArtifact::Lakehouse,
            operation,
        )
    }
}
