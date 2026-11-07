// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parent-owned table operation descriptions, independent of any execution head.
//!
//! These describe SDK bindings, not configured CLI capabilities, provider
//! permissions or live availability. A host must inject the matching executor
//! and resolve the exact parent and table before advertising an executable
//! operation. No table-copy, upload or engine fallback is implied.

use serde_json::{Value, json};

/// The artifact that owns a table's identity and native service binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParentArtifact {
    /// A Lakehouse, with its own schema and Delta maintenance capabilities.
    Lakehouse,
    /// A KQLDatabase, with native Kusto table and ingestion semantics.
    KqlDatabase,
    /// A Warehouse, with native TDS and COPY INTO semantics.
    Warehouse,
}

impl ParentArtifact {
    /// Returns the parent artifact's service type, not a standalone table type.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lakehouse => "Lakehouse",
            Self::KqlDatabase => "KQLDatabase",
            Self::Warehouse => "Warehouse",
        }
    }
}

/// A table operation or read representation within a parent artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableOperation {
    /// Retrieve the backend's explicitly identified schema representation.
    GetSchema,
    /// Retrieve statistics with native provenance and freshness qualifications.
    GetStatistics,
    /// Execute a read through the parent's query binding.
    Query,
    /// Load data using this parent's native source and mode contract.
    Load,
    /// Optimize a Lakehouse Delta table.
    Optimize,
    /// Remove historical Lakehouse Delta files under an explicit retention policy.
    Vacuum,
    /// Copy a table with a declared consistency contract; not yet supported.
    Copy,
}

impl TableOperation {
    /// Returns the operation spelling; schema/statistics are get representations.
    pub const fn verb(self) -> &'static str {
        match self {
            Self::GetSchema | Self::GetStatistics => "get",
            Self::Query => "query",
            Self::Load => "load",
            Self::Optimize => "optimize",
            Self::Vacuum => "vacuum",
            Self::Copy => "copy",
        }
    }

    /// Returns the read representation when the operation is a get.
    pub const fn representation(self) -> Option<&'static str> {
        match self {
            Self::GetSchema => Some("schema"),
            Self::GetStatistics => Some("statistics"),
            _ => None,
        }
    }
}

/// Whether this SDK contains the operation or needs an additional native binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingSupport {
    /// An SDK adapter exists; the caller must still supply its execution boundary.
    Adapter,
    /// The operation requires a separate binding not supplied by this slice.
    AdditionalBindingRequired,
    /// The operation is deliberately unsupported for this parent.
    Unsupported,
}

/// A native protocol; similar verbs never make these transports interchangeable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableTransport {
    /// Bounded OneLake JSON transaction-log reads, not a full Delta protocol reader.
    DeltaLog,
    /// The Fabric Load Table API and its returned operation monitor.
    FabricLoad,
    /// A Lakehouse maintenance job, observed through the generic item job API.
    FabricJob,
    /// A native Kusto management command.
    KustoManagement,
    /// The Kusto v2 read-query protocol.
    KustoQuery,
    /// An encrypted TDS session bound to the exact Warehouse catalog.
    Tds,
}

/// A parent-specific operation description shared by SDK/discovery consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableBinding {
    /// The owning artifact; always retained when selecting a native adapter.
    pub parent: ParentArtifact,
    /// The selected operation or get representation.
    pub operation: TableOperation,
    /// SDK support, not host configuration or provider permission.
    pub support: BindingSupport,
    /// The native transport, when a binding is defined.
    pub transport: Option<TableTransport>,
    /// Source, effect and completion qualifications that hosts must retain.
    pub qualification: &'static str,
}

/// Describe one operation without acquiring credentials or performing any I/O.
///
/// Unsupported or unbound operations must not silently switch engines, upload
/// sources, drop schemas, change identities or invoke another CLI command.
pub const fn describe(parent: ParentArtifact, operation: TableOperation) -> TableBinding {
    use BindingSupport::{Adapter, AdditionalBindingRequired, Unsupported};
    use ParentArtifact::{KqlDatabase, Lakehouse, Warehouse};
    use TableOperation::{Copy, GetSchema, GetStatistics, Load, Optimize, Query, Vacuum};
    use TableTransport::{DeltaLog, FabricJob, FabricLoad, KustoManagement, KustoQuery, Tds};

    let (support, transport, qualification) = match (parent, operation) {
        (_, Copy) => (
            Unsupported,
            None,
            "No snapshot-consistent table-copy contract is implemented; file copy is not table copy.",
        ),
        (Lakehouse, GetSchema) => (
            Adapter,
            Some(DeltaLog),
            "JSON-log compatibility schema only; checkpoint-only tables require another binding.",
        ),
        (Lakehouse, GetStatistics) => (
            AdditionalBindingRequired,
            None,
            "Catalog snapshot statistics and SQL-endpoint health require explicit separate bindings.",
        ),
        (Lakehouse, Query) => (
            AdditionalBindingRequired,
            None,
            "Resolve an explicit parent query binding; storage and SQL engines are not fallbacks.",
        ),
        (Lakehouse, Load) => (
            Adapter,
            Some(FabricLoad),
            "Existing same-item OneLake Files source; explicit append/overwrite; accepted is not completed.",
        ),
        (Lakehouse, Optimize) => (
            Adapter,
            Some(FabricJob),
            "One exact Delta table; V-Order is opt-in; job acceptance is not completion.",
        ),
        (Lakehouse, Vacuum) => (
            Adapter,
            Some(FabricJob),
            "Historical-file deletion; minimum retention is 168 hours; no workspace safety-setting changes.",
        ),
        (KqlDatabase, GetSchema | GetStatistics) => (
            Adapter,
            Some(KustoManagement),
            "Native metadata; statistics retain cached freshness and unavailable fields.",
        ),
        (KqlDatabase, Query) => (
            Adapter,
            Some(KustoQuery),
            "Native parent read context, not single-table isolation; inspect terminal error/completion frames.",
        ),
        (KqlDatabase, Load) => (
            Adapter,
            Some(KustoManagement),
            "Explicit inline CSV or storage pull, append-only; preserve extent errors and partial outcomes.",
        ),
        (Warehouse, GetSchema | GetStatistics) => (
            Adapter,
            Some(Tds),
            "Exact catalog/schema/table; preserve native types and all result sets.",
        ),
        (Warehouse, Query) => (
            Adapter,
            Some(Tds),
            "Requires an enforced native read binding, not SQL-prefix inspection or table-isolation claims.",
        ),
        (Warehouse, Load) => (
            Adapter,
            Some(Tds),
            "COPY INTO appends to an existing table; terminal SQL evidence, not a Fabric job or LRO.",
        ),
        (KqlDatabase | Warehouse, Optimize | Vacuum) => (
            Unsupported,
            None,
            "Lakehouse Delta maintenance is not a generic database optimization or retention operation.",
        ),
    };
    TableBinding {
        parent,
        operation,
        support,
        transport,
        qualification,
    }
}

/// Produce a stable machine description without claiming a configured executor.
pub fn describe_json(parent: ParentArtifact, operation: TableOperation) -> Value {
    let binding = describe(parent, operation);
    let support = match binding.support {
        BindingSupport::Adapter => "sdkAdapter",
        BindingSupport::AdditionalBindingRequired => "additionalBindingRequired",
        BindingSupport::Unsupported => "unsupported",
    };
    let transport = binding.transport.map(|transport| match transport {
        TableTransport::DeltaLog => "oneLakeDeltaLog",
        TableTransport::FabricLoad => "fabricLoadOperation",
        TableTransport::FabricJob => "fabricItemJob",
        TableTransport::KustoManagement => "kustoManagement",
        TableTransport::KustoQuery => "kustoQuery",
        TableTransport::Tds => "tds",
    });
    json!({
        "parentType": parent.name(),
        "subresourceType": "table",
        "verb": operation.verb(),
        "representation": operation.representation(),
        "support": support,
        "transport": transport,
        "requiresInjectedExecutor": binding.support == BindingSupport::Adapter,
        "qualification": binding.qualification,
    })
}
