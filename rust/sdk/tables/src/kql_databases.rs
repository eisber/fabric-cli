// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! KQLDatabase-owned subresources using the native Kusto protocol.

/// Native table metadata, read-query and explicit append-only ingestion adapters.
pub mod tables {
    pub use crate::capabilities::TableOperation;
    pub use crate::kql_database::*;

    /// Describe a KQLDatabase table operation without configuring service execution.
    pub fn describe(operation: TableOperation) -> serde_json::Value {
        crate::capabilities::describe_json(
            crate::capabilities::ParentArtifact::KqlDatabase,
            operation,
        )
    }
}
