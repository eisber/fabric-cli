// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated Livy JSON bodies and profile-specific query parameters.

use super::segment;
use crate::runtime::NativeError;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::fmt;

fn nonblank(value: &str) -> Result<(), NativeError> {
    if value.trim().is_empty() {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}

fn integer(value: u64) -> Result<(), NativeError> {
    if value > i64::MAX as u64 {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}

macro_rules! body {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, PartialEq)]
        pub struct $name(Value);

        impl $name {
            /// Borrow the exact wire body. Never log executable or serialized content.
            pub fn as_json(&self) -> &Value {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

body!(
    Configuration,
    "Extensible object for session, batch, REPL or HC creation."
);
body!(
    StatementInput,
    "Code and interpreter kind; the JSON default is `sql`."
);
body!(
    CompletionInput,
    "Completion code, Unicode-scalar cursor, kind and optional source ID."
);
body!(
    InspectionInput,
    "Variable selectors, kind and optional explicit source ID."
);
body!(
    DebugInput,
    "Opaque Jupyter debug request in the native Livy debug envelope."
);
body!(
    TimeoutInput,
    "Positive millisecond timeout changes, not activity reset."
);
body!(
    SerializedJob,
    "Opaque already-serialized job bytes encoded as base64, never deserialized."
);
body!(
    ResourceUri,
    "Server-localized resource URI; the SDK never fetches this URI."
);

impl Configuration {
    /// Preserve an object, including unknown deployment fields, without imposing a Spark schema.
    ///
    /// # Errors
    /// Rejects non-object JSON. Session/batch creation intentionally has an open schema.
    pub fn new(value: Value) -> Result<Self, NativeError> {
        if !value.is_object() {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self(value))
    }

    pub(super) fn validate_hc(&self) -> Result<(), NativeError> {
        for key in ["sessionTag", "name", "kind"] {
            if self.0.get(key).is_some_and(|value| !value.is_string()) {
                return Err(NativeError::InvalidInput);
            }
        }
        if self.0.get("conf").is_some_and(|value| !value.is_object()) {
            return Err(NativeError::InvalidInput);
        }
        Ok(())
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self(json!({}))
    }
}

impl StatementInput {
    /// Build a JSON statement with the pinned default interpreter, `sql`.
    ///
    /// # Errors
    /// Rejects blank code.
    pub fn new(code: &str) -> Result<Self, NativeError> {
        nonblank(code)?;
        Ok(Self(json!({"code": code, "kind": "sql"})))
    }

    /// Select an explicit interpreter without changing the owner or route.
    ///
    /// # Errors
    /// Rejects a blank kind.
    pub fn with_kind(mut self, kind: &str) -> Result<Self, NativeError> {
        nonblank(kind)?;
        self.0["kind"] = kind.into();
        Ok(self)
    }

    /// Validate the closed statement schema, applying `kind=sql` only when absent.
    ///
    /// # Errors
    /// Rejects unknown fields, wrong types, blank code/kind and implicit source-ID routing.
    pub fn from_json(value: Value) -> Result<Self, NativeError> {
        let object = value.as_object().ok_or(NativeError::InvalidInput)?;
        if object.keys().any(|key| key != "code" && key != "kind") {
            return Err(NativeError::InvalidInput);
        }
        let input = Self::new(
            object
                .get("code")
                .and_then(Value::as_str)
                .ok_or(NativeError::InvalidInput)?,
        )?;
        match object.get("kind") {
            None => Ok(input),
            Some(Value::String(kind)) => input.with_kind(kind),
            Some(_) => Err(NativeError::InvalidInput),
        }
    }
}

impl CompletionInput {
    /// Build completion input with `kind=spark`.
    ///
    /// The cursor follows Python's Unicode-scalar indexing, not UTF-8 bytes.
    ///
    /// # Errors
    /// Rejects a cursor beyond the end of code or the signed wire integer range.
    pub fn new(code: &str, cursor: u64) -> Result<Self, NativeError> {
        integer(cursor)?;
        if cursor > code.chars().count() as u64 {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self(
            json!({"kind": "spark", "code": code, "cursor": cursor}),
        ))
    }

    /// Set an explicit interpreter.
    ///
    /// # Errors
    /// Rejects a blank kind.
    pub fn with_kind(mut self, kind: &str) -> Result<Self, NativeError> {
        nonblank(kind)?;
        self.0["kind"] = kind.into();
        Ok(self)
    }

    /// Set the explicitly selected native REPL source ID (never statement routing).
    ///
    /// # Errors
    /// Rejects unsafe IDs.
    pub fn with_source_id(mut self, source_id: &str) -> Result<Self, NativeError> {
        segment(source_id)?;
        self.0["sourceId"] = source_id.into();
        Ok(self)
    }
}

impl InspectionInput {
    /// Build variable inspection with `kind=spark`; an empty selection is valid.
    ///
    /// # Errors
    /// Rejects empty variable names.
    pub fn new(variables: &[&str]) -> Result<Self, NativeError> {
        if variables.iter().any(|value| value.is_empty()) {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self(json!({"kind": "spark", "vars": variables})))
    }

    /// Set an explicit interpreter.
    ///
    /// # Errors
    /// Rejects a blank kind.
    pub fn with_kind(mut self, kind: &str) -> Result<Self, NativeError> {
        nonblank(kind)?;
        self.0["kind"] = kind.into();
        Ok(self)
    }

    /// Set an explicit native REPL source ID.
    ///
    /// # Errors
    /// Rejects unsafe IDs.
    pub fn with_source_id(mut self, source_id: &str) -> Result<Self, NativeError> {
        segment(source_id)?;
        self.0["sourceId"] = source_id.into();
        Ok(self)
    }
}

impl Default for InspectionInput {
    fn default() -> Self {
        Self(json!({"kind": "spark", "vars": []}))
    }
}

impl DebugInput {
    /// Wrap an object or already-serialized string in `debug_request`.
    ///
    /// Debug uses the server's default REPL; no source ID is synthesized.
    ///
    /// # Errors
    /// Rejects non-object/non-string content or serialization failure.
    pub fn new(content: Value) -> Result<Self, NativeError> {
        let content = match content {
            Value::String(value) => value,
            Value::Object(_) => {
                serde_json::to_string(&content).map_err(|_| NativeError::InvalidInput)?
            }
            _ => return Err(NativeError::InvalidInput),
        };
        Ok(Self(
            json!({"msg_type": "debug_request", "content": content}),
        ))
    }
}

impl TimeoutInput {
    /// Set the classic session timeout in positive milliseconds.
    ///
    /// # Errors
    /// Rejects zero or values outside the signed wire integer range.
    pub fn session(milliseconds: u64) -> Result<Self, NativeError> {
        Self::repl(Some(milliseconds), None)
    }

    /// Set either or both session and REPL timeout durations.
    ///
    /// # Errors
    /// Requires at least one positive signed-64-bit value.
    pub fn repl(session: Option<u64>, repl: Option<u64>) -> Result<Self, NativeError> {
        if session.is_none() && repl.is_none() {
            return Err(NativeError::InvalidInput);
        }
        let mut value = json!({});
        for (key, milliseconds) in [("sessionTimeout", session), ("replTimeout", repl)] {
            if let Some(milliseconds) = milliseconds {
                integer(milliseconds)?;
                if milliseconds == 0 {
                    return Err(NativeError::InvalidInput);
                }
                value[key] = milliseconds.into();
            }
        }
        Ok(Self(value))
    }
}

impl SerializedJob {
    /// Encode opaque bytes exactly once and retain the explicit `jobType`.
    ///
    /// # Errors
    /// Rejects empty bytes or a blank type. No serializer or application execution is installed.
    pub fn new(bytes: &[u8], job_type: &str) -> Result<Self, NativeError> {
        nonblank(job_type)?;
        if bytes.is_empty() {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self(
            json!({"job": STANDARD.encode(bytes), "jobType": job_type}),
        ))
    }
}

impl ResourceUri {
    /// Preserve the URI for server-side localization, without local URI resolution.
    ///
    /// # Errors
    /// Rejects blank strings and controls.
    pub fn new(uri: &str) -> Result<Self, NativeError> {
        nonblank(uri)?;
        if uri.chars().any(char::is_control) {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self(json!({"uri": uri})))
    }
}

/// Profile-specific activity-list parameters. No cross-profile coercion is performed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ActivityPage {
    /// Omit all optional parameters for the selected profile.
    #[default]
    Default,
    /// Native `from`/`size`.
    Native {
        /// Byte-independent activity offset.
        offset: Option<u64>,
        /// Maximum activity entries.
        size: Option<u32>,
    },
    /// Fabric `$top`/`$skip`/`$count`.
    Fabric {
        /// Maximum entries.
        top: Option<u32>,
        /// Entries to skip.
        skip: Option<u32>,
        /// Request total count.
        count: Option<bool>,
    },
}

/// Optional `from`/`size`, used for logs or one statement byte page.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BytePage {
    offset: Option<u64>,
    size: Option<u32>,
}

impl BytePage {
    /// Validate optional nonnegative wire integers. Zero sizes match explicit single-read source input.
    ///
    /// # Errors
    /// Rejects offsets above int64 or sizes above int32.
    pub fn new(offset: Option<u64>, size: Option<u32>) -> Result<Self, NativeError> {
        if let Some(offset) = offset {
            integer(offset)?;
        }
        if size.is_some_and(|size| size > i32::MAX as u32) {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self { offset, size })
    }

    pub(super) fn query(self) -> Vec<(&'static str, String)> {
        let mut query = Vec::new();
        if let Some(offset) = self.offset {
            query.push(("from", offset.to_string()));
        }
        if let Some(size) = self.size {
            query.push(("size", size.to_string()));
        }
        query
    }
}

/// Native-only statement list parameters; Fabric supports only the empty default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StatementListOptions {
    page: BytePage,
    tail: Option<u32>,
}

impl StatementListOptions {
    /// Validate native list offset, size and tail.
    ///
    /// # Errors
    /// Rejects out-of-range wire integers.
    pub fn new(
        offset: Option<u64>,
        size: Option<u32>,
        tail: Option<u32>,
    ) -> Result<Self, NativeError> {
        if tail.is_some_and(|tail| tail > i32::MAX as u32) {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self {
            page: BytePage::new(offset, size)?,
            tail,
        })
    }

    pub(super) fn query(self) -> Vec<(&'static str, String)> {
        let mut query = self.page.query();
        if let Some(tail) = self.tail {
            query.push(("tail", tail.to_string()));
        }
        query
    }
}

/// Native REPL list offset/take pair and optional server-defined filter.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct ReplListOptions {
    query: Vec<(&'static str, String)>,
}

impl ReplListOptions {
    /// Validate paired pagination and an optional literal filter.
    ///
    /// # Errors
    /// Rejects unpaired pagination, out-of-range numbers and filter controls.
    pub fn new(
        offset: Option<u64>,
        take: Option<u32>,
        filter: Option<&str>,
    ) -> Result<Self, NativeError> {
        if offset.is_some() != take.is_some() {
            return Err(NativeError::InvalidInput);
        }
        let mut query = BytePage::new(offset, take)?.query();
        for (key, _) in &mut query {
            if *key == "size" {
                *key = "take";
            }
        }
        if let Some(filter) = filter {
            if filter.chars().any(char::is_control) {
                return Err(NativeError::InvalidInput);
            }
            query.push(("filter", filter.into()));
        }
        Ok(Self { query })
    }

    pub(super) fn query(&self) -> Vec<(&'static str, String)> {
        self.query.clone()
    }
}

impl fmt::Debug for ReplListOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReplListOptions(<redacted>)")
    }
}

pub(super) fn activity_query(
    page: ActivityPage,
    native: bool,
) -> Result<Vec<(&'static str, String)>, NativeError> {
    match (page, native) {
        (ActivityPage::Default, _) => Ok(Vec::new()),
        (ActivityPage::Native { offset, size }, true) => Ok(BytePage::new(offset, size)?.query()),
        (ActivityPage::Fabric { top, skip, count }, false) => {
            if top.is_some_and(|n| n > i32::MAX as u32) || skip.is_some_and(|n| n > i32::MAX as u32)
            {
                return Err(NativeError::InvalidInput);
            }
            let mut query = Vec::new();
            for (key, value) in [("$top", top), ("$skip", skip)] {
                if let Some(value) = value {
                    query.push((key, value.to_string()));
                }
            }
            if let Some(count) = count {
                query.push(("$count", count.to_string()));
            }
            Ok(query)
        }
        _ => Err(NativeError::UnsupportedCapability),
    }
}
