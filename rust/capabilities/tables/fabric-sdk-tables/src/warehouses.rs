// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse-owned subresources, bound to the exact SQL catalog.

/// Native table metadata, read-query and existing-target COPY INTO adapters.
pub mod tables {
    pub use crate::capabilities::TableOperation;
    pub use crate::warehouse::*;

    /// Describe a Warehouse table operation without configuring service execution.
    pub fn describe(operation: TableOperation) -> serde_json::Value {
        crate::capabilities::describe_json(
            crate::capabilities::ParentArtifact::Warehouse,
            operation,
        )
    }
}
