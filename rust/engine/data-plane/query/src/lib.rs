// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit engine bindings and read-intent KQL over caller-owned native streams.
//!
//! [`KustoQueryExecutor`] composes an injected credential, shared streaming HTTP
//! stack and caller timer with an exact engine binding and fixed read policy.
//! Structural qualification is not live tenant-enforcement proof. No credential,
//! runtime, HTTP client, retry or cancellation command is created; JMESPath
//! projection is not an engine or a safety check.

mod binding;
mod kql;
mod policy;
mod production;
mod result;

pub use binding::{EngineBinding, EngineExecutor, QueryCapability};
pub use kql::{KqlQuery, QueryError, QueryErrorKind, QueryLimits, query_kql};
pub use production::{KustoCredentialContext, KustoQueryExecutor};
pub use result::{
    QueryColumn, QueryDataset, QueryDiagnostic, QueryDiagnosticKind, QueryEvidence, QueryResult,
};
