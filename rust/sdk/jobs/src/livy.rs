// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure existing-session Livy statement input and native result interpretation.
//!
//! No endpoint, credential, session selection, HTTP execution or lifecycle
//! authority is inferred here. A production consumer must use the released
//! Lakehouse binding and shared run deadline/evidence interfaces.

use serde_json::{Value, json};
use std::fmt;

/// Explicit supported source language; SQL also has execution effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    /// Python on Spark.
    PySpark,
    /// Scala on Spark.
    Scala,
    /// Spark SQL, not a read-only query contract.
    Sql,
    /// R on Spark.
    R,
}

impl Language {
    /// Exact native Livy kind, without aliases or language inference.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::PySpark => "pyspark",
            Self::Scala => "spark",
            Self::Sql => "sql",
            Self::R => "sparkr",
        }
    }
}

/// Locally validated code and language, with no ambient file interpretation.
#[derive(Clone, Eq, PartialEq)]
pub struct StatementInput {
    code: String,
    language: Language,
}

impl StatementInput {
    /// Validate the closed `{code, language}` input object without any I/O.
    ///
    /// Code is preserved byte-for-byte. A `kind` override, cell input, or any
    /// other field is unsupported rather than translated into execution.
    pub fn from_json(input: &Value) -> Result<Self, InputError> {
        let fields = input.as_object().ok_or(InputError::ObjectRequired)?;
        if fields.keys().any(|key| key != "code" && key != "language") {
            return Err(InputError::UnsupportedField);
        }
        let code = fields
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| !code.trim().is_empty())
            .ok_or(InputError::CodeRequired)?;
        let language = match fields.get("language").and_then(Value::as_str) {
            Some("pyspark") => Language::PySpark,
            Some("scala") => Language::Scala,
            Some("sql") => Language::Sql,
            Some("r") => Language::R,
            _ => return Err(InputError::LanguageRequired),
        };
        Ok(Self {
            code: code.to_owned(),
            language,
        })
    }

    /// Explicit language selected by the caller.
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Exact native POST body, containing only `code` and `kind`.
    ///
    /// This contains caller code and must use protected request/result paths,
    /// never incidental diagnostics.
    pub fn body(&self) -> Value {
        json!({"code": self.code, "kind": self.language.kind()})
    }
}

impl fmt::Debug for StatementInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatementInput")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

/// Sanitized pure-input failure; supplied code and field values are not echoed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InputError {
    /// An input JSON object is required.
    #[error("Livy statement input must be an object.")]
    ObjectRequired,
    /// Unknown fields have no implicit execution interpretation.
    #[error("Livy statement input contains an unsupported field.")]
    UnsupportedField,
    /// Code must be a nonblank string.
    #[error("Livy statement input requires nonblank code.")]
    CodeRequired,
    /// Language is required and has no default.
    #[error("Livy statement language must be pyspark, scala, sql or r.")]
    LanguageRequired,
}

/// Actual native statement identifier, retaining JSON string/number identity.
///
/// This is not a session, application, attempt, acquisition, REPL or scheduler
/// identifier. It does not by itself establish owner or endpoint scope.
#[derive(Clone, Eq, PartialEq)]
pub struct StatementId(Value);

impl StatementId {
    /// Validate a native nonnegative integer or nonblank string ID.
    ///
    /// Numeric zero is valid. Numeric and string spellings are not conflated.
    pub fn from_json(id: &Value) -> Result<Self, ResponseError> {
        let valid = match id {
            Value::Number(number) => number.as_u64().is_some(),
            Value::String(text) => !text.trim().is_empty(),
            _ => false,
        };
        if !valid {
            return Err(ResponseError::StatementIdRequired);
        }
        Ok(Self(id.clone()))
    }

    /// Actual protected native ID value, not a generated or normalized ID.
    pub fn native(&self) -> &Value {
        &self.0
    }
}

impl fmt::Debug for StatementId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatementId").finish_non_exhaustive()
    }
}

/// Interpretation of actual statement state *and* output status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatementVerdict {
    /// Waiting/running with no output verdict; acceptance is not success.
    Pending,
    /// Exact `available` state and `output.status=ok`.
    Succeeded,
    /// Native error state or error output; preserved error fields are evidence.
    Failed,
    /// Exact native cancelled state; not rollback or session termination.
    Cancelled,
    /// Missing, unknown or contradictory state/output cannot verify success.
    Unverified,
}

/// Native response retained for common protected result/evidence projection.
///
/// Decoding a successful HTTP response does not establish execution success.
/// The common run consumer must separately retain acceptance, scoped handle,
/// mutation uncertainty and local-delivery errors.
#[derive(Clone, PartialEq)]
pub struct StatementObservation {
    id: StatementId,
    response: Value,
}

impl StatementObservation {
    /// Retain the exact native object and actual statement identity.
    ///
    /// Missing/malformed IDs after POST imply an unknown possible effect; this
    /// error is not permission to resubmit or find a newest statement.
    pub fn from_json(response: Value) -> Result<Self, ResponseError> {
        let fields = response.as_object().ok_or(ResponseError::ObjectRequired)?;
        let id =
            StatementId::from_json(fields.get("id").ok_or(ResponseError::StatementIdRequired)?)?;
        Ok(Self { id, response })
    }

    /// Actual native statement ID; numeric `0` remains present.
    pub const fn id(&self) -> &StatementId {
        &self.id
    }

    /// Full native evidence including media, error, traceback and unknown fields.
    ///
    /// Caller-selected output is a protected result surface. Do not log or
    /// interpolate this untrusted provider data into incidental diagnostics.
    pub const fn native_response(&self) -> &Value {
        &self.response
    }

    /// Actual output value; absent and explicit null remain distinct.
    pub fn output(&self) -> Option<&Value> {
        self.response.get("output")
    }

    /// Full media map, with JSON/false/zero/null values preserved as supplied.
    ///
    /// Missing/malformed media is not silently converted to an empty map.
    pub fn media(&self) -> Option<&serde_json::Map<String, Value>> {
        self.output()?.get("data")?.as_object()
    }

    /// Native state and output must agree before execution success is verified.
    pub fn verdict(&self) -> StatementVerdict {
        let state = self.response.get("state").and_then(Value::as_str);
        let output = self.output().filter(|output| !output.is_null());
        let status = output
            .and_then(|output| output.get("status"))
            .and_then(Value::as_str);
        match (state, status) {
            (Some("cancelled"), None | Some("error")) => StatementVerdict::Cancelled,
            (Some("error"), None | Some("error")) | (Some("available"), Some("error")) => {
                StatementVerdict::Failed
            }
            (Some("available"), Some("ok")) => StatementVerdict::Succeeded,
            (Some("waiting" | "running"), None) if output.is_none() => StatementVerdict::Pending,
            _ => StatementVerdict::Unverified,
        }
    }

    /// Explicit native truncation/partial evidence in output or a media envelope.
    ///
    /// A terminal successful execution with partial data is not exhaustive result
    /// delivery. Absence of these flags does not override transport/sink bounds.
    pub fn has_partial_output(&self) -> bool {
        fn partial(value: &Value) -> bool {
            ["truncated", "partial"]
                .iter()
                .any(|key| value.get(key).and_then(Value::as_bool) == Some(true))
        }
        self.output().is_some_and(partial)
            || self
                .media()
                .is_some_and(|media| media.values().any(partial))
    }
}

impl fmt::Debug for StatementObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatementObservation")
            .field("verdict", &self.verdict())
            .field("partial_output", &self.has_partial_output())
            .finish_non_exhaustive()
    }
}

/// Sanitized response-shape failure, retaining no incidental provider text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResponseError {
    /// Native statement response must be an object.
    #[error("Livy statement response must be an object.")]
    ObjectRequired,
    /// No scoped statement handle may be fabricated from a missing/invalid ID.
    #[error("Livy statement response requires a native statement ID.")]
    StatementIdRequired,
}
