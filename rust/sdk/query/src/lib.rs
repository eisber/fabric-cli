// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit engine bindings and read-intent KQL over caller-owned native streams.
//!
//! Scripted exchanges exercise the real request/decoder offline. Production
//! remains gated until the native audience and read-enforcement adapter is
//! verified. No credentials, runtime, HTTP client, retry or cancellation command
//! is created; JMESPath projection is not an engine or a safety check.

mod binding;
mod kql;
mod result;

pub use binding::{EngineBinding, EngineExecutor, QueryCapability};
pub use kql::{KqlQuery, QueryError, QueryErrorKind, QueryLimits, query_kql};
pub use result::{QueryColumn, QueryDataset, QueryEvidence, QueryResult};
