// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_output::CommandError;
use serde_json::Value;

/// Host-injected Python query/path-discovery boundary, separate from JSONPath mutation.
pub trait Projection: Send + Sync {
    /// Project sorted LS rows or raw GET metadata.
    ///
    /// `None` on GET means Python's discoverable JSON paths, not identity.
    /// No-query LS uses source column projection without invoking this method.
    fn project(
        &self,
        data: Value,
        expression: Option<&str>,
        deep_traversal: bool,
    ) -> Result<Value, CommandError>;
}

/// Built-in Python-compatible query and GET path-discovery implementation.
///
/// The same native evaluator serves both commands; GET with no expression uses
/// path discovery, while no-query LS never invokes this provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct PythonQuery;

impl Projection for PythonQuery {
    fn project(
        &self,
        data: Value,
        expression: Option<&str>,
        deep_traversal: bool,
    ) -> Result<Value, CommandError> {
        fabric_cli_query::search_with_options(
            &data,
            expression.unwrap_or(""),
            fabric_cli_query::SearchOptions { deep_traversal },
        )
        .map_err(|error| CommandError::new(Some(error.message().into()), Some(error.code().into())))
    }
}

/// Explicit opt-out for hosts that do not enable query evaluation.
///
/// No-query LS is fully executable. Queries and GET path discovery explicitly
/// fail instead of pretending that raw data is Python's query result.
pub struct NoQuery;

impl Projection for NoQuery {
    fn project(&self, _: Value, _: Option<&str>, _: bool) -> Result<Value, CommandError> {
        Err(crate::errors::unsupported(
            "Query projection and GET path discovery require a configured projection provider",
        ))
    }
}
