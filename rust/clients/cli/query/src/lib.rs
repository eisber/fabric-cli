// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure CLI JMESPath search, distinct from deployment JSONPath mutation.
//!
//! [`search`] translates the Python wrapper, including empty-query path inventory
//! and scalar stringification. [`project`] additionally distinguishes absent queries.
//! All inputs are caller-owned JSON; this library does no I/O or ambient discovery.
//!
//! ```
//! use fabric_cli_query::{project, search};
//! use serde_json::json;
//! let data = json!([{"id": "a", "n": 2}, {"id": "b", "n": 0}]);
//! assert_eq!(search(&data, "[?n > `1`].id")?, json!(["a"]));
//! assert_eq!(project(&data, None)?, data);
//! # Ok::<(), fabric_cli_query::QueryError>(())
//! ```

mod functions;
mod scalar;
mod validation;

use jpx_core::{ErrorReason, RuntimeError};
use serde_json::Value;
use thiserror::Error;

/// A query failed; callers retain control of output and exit selection.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("Invalid jmespath query (https://jmespath.org)")]
pub struct QueryError {
    kind: QueryErrorKind,
}

/// Stable error categories without embedding the query or queried data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryErrorKind {
    /// Malformed JMESPath.
    Syntax,
    /// A function received incompatible types.
    InvalidType,
    /// A function received the wrong number of arguments.
    InvalidArity,
    /// A function is not part of the Python JMESPath runtime.
    UnknownFunction,
    /// A known source operation cannot be reproduced within this JSON boundary.
    Unsupported,
    /// The underlying parser or interpreter rejected excessive nesting.
    ResourceLimit,
    /// Another invalid runtime operation, such as a slice with zero step.
    Evaluation,
}

impl QueryError {
    /// Inspect the semantic category; callers must not treat an error as empty data.
    pub fn kind(&self) -> QueryErrorKind {
        self.kind
    }

    /// Python's structured CLI error code.
    pub fn code(&self) -> &'static str {
        "InvalidInput"
    }

    /// Python's public UI message, without query or data disclosure.
    pub fn message(&self) -> &'static str {
        "Invalid jmespath query (https://jmespath.org)"
    }
}

impl From<jpx_core::JmespathError> for QueryError {
    fn from(error: jpx_core::JmespathError) -> Self {
        let kind = match error.reason {
            ErrorReason::Parse(message) if message.starts_with("unsupported:") => {
                QueryErrorKind::Unsupported
            }
            ErrorReason::Parse(message) if message.contains("maximum depth") => {
                QueryErrorKind::ResourceLimit
            }
            ErrorReason::Parse(message) if message == "Expected valid number" => {
                QueryErrorKind::Unsupported
            }
            ErrorReason::Parse(_) => QueryErrorKind::Syntax,
            ErrorReason::Runtime(
                RuntimeError::InvalidType { .. } | RuntimeError::InvalidReturnType { .. },
            ) => QueryErrorKind::InvalidType,
            ErrorReason::Runtime(
                RuntimeError::NotEnoughArguments { .. } | RuntimeError::TooManyArguments { .. },
            ) => QueryErrorKind::InvalidArity,
            ErrorReason::Runtime(RuntimeError::UnknownFunction(_)) => {
                QueryErrorKind::UnknownFunction
            }
            ErrorReason::Runtime(RuntimeError::RecursionLimitExceeded { .. }) => {
                QueryErrorKind::ResourceLimit
            }
            _ => QueryErrorKind::Evaluation,
        };
        Self { kind }
    }
}

fn unsupported() -> QueryError {
    QueryError {
        kind: QueryErrorKind::Unsupported,
    }
}

/// Evaluate a CLI query, preserving `.` identity and Python scalar strings.
pub fn search(data: &Value, expression: &str) -> Result<Value, QueryError> {
    search_with_options(data, expression, SearchOptions::default())
}

/// Path-inventory options corresponding to Python's `deep_traversal` argument.
#[derive(Clone, Copy, Debug, Default)]
pub struct SearchOptions {
    /// Traverse all JSON levels instead of the source's default maximum depth four.
    pub deep_traversal: bool,
}

/// Preserve unqueried data; an explicitly empty query still inventories paths.
pub fn project(data: &Value, expression: Option<&str>) -> Result<Value, QueryError> {
    expression.map_or_else(|| Ok(data.clone()), |expression| search(data, expression))
}

/// Join argparse-style query tokens using Python `process_nargs` list semantics.
///
/// A caller with an already-normalized single string should pass it directly to
/// [`search`]: Python does not strip quotes from its single-string branch.
pub fn join_query_args(tokens: &[impl AsRef<str>]) -> String {
    tokens
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['"', '\''])
        .to_owned()
}

/// Evaluate a query with explicit path-inventory depth behavior.
pub fn search_with_options(
    data: &Value,
    expression: &str,
    options: SearchOptions,
) -> Result<Value, QueryError> {
    if expression.is_empty() {
        return Ok(paths(data, options.deep_traversal));
    }
    if expression == "." {
        return Ok(data.clone());
    }
    validation::literals(expression)?;
    let mut ast = jpx_core::parse(expression).map_err(QueryError::from)?;
    let numeric_sensitive = validation::validate(&ast)?;
    validation::adapt_comparisons(&mut ast);
    let runtime = functions::runtime(numeric_sensitive);
    let compiled = jpx_core::Expression::new(expression, ast, &runtime);
    let result = compiled.search(data).map_err(QueryError::from)?;
    Ok(match result {
        Value::Array(_) | Value::Object(_) | Value::String(_) => result,
        Value::Null => Value::String("None".to_owned()),
        Value::Bool(true) => Value::String("True".to_owned()),
        Value::Bool(false) => Value::String("False".to_owned()),
        Value::Number(number) => Value::String(scalar::number(&number)),
    })
}

fn paths(data: &Value, deep: bool) -> Value {
    let mut paths = Vec::new();
    let mut pending = vec![(data, String::new(), 0, false)];
    while let Some((value, path, depth, terminal)) = pending.pop() {
        if terminal {
            paths.push(Value::String(path));
            continue;
        }
        let terminal = !deep && depth >= 4;
        match value {
            Value::Object(map) => {
                for (key, value) in map.iter().rev() {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    pending.push((value, path, depth + 1, terminal));
                }
            }
            Value::Array(values) if !values.is_empty() => {
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push((value, format!("{path}[{index}]"), depth + 1, terminal));
                }
            }
            _ => paths.push(Value::String(path)),
        }
    }
    Value::Array(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn candidate_preserves_order_numbers_and_real_operators() {
        let data: Value = serde_json::from_str(
            r#"{"z":1.0,"a":9007199254740993,"rows":[{"id":"b","n":2},{"id":"a","n":1}]}"#,
        )
        .unwrap();
        assert_eq!(search(&data, "keys(@)").unwrap(), json!(["z", "a", "rows"]));
        assert_eq!(search(&data, "z").unwrap(), json!("1.0"));
        assert_eq!(search(&data, "a").unwrap(), json!("9007199254740993"));
        assert_eq!(
            search(
                &data,
                "rows[?n > `0`] | sort_by(@, &n)[].{key: id, count: n}"
            )
            .unwrap()
            .to_string(),
            r#"[{"key":"a","count":1},{"key":"b","count":2}]"#
        );
        assert!(search(&data, "length(`1`)").is_err());
        assert!(search(&data, "[?").is_err());
    }
}
