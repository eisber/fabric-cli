// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure JSON output translation from Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
//!
//! Callers supply timestamps and already-projected JSON, then inspect a [`RenderPlan`]
//! or write it to caller-owned streams. No terminal, environment, clock, logger,
//! dispatcher, SDK, or process-global state is accessed. See the crate README for
//! the bounded numeric and Python-runtime compatibility contract.
//!
//! ```
//! use fabric_cli_output::{Indent, OutputEnvelope, OutputInput, Timestamp, parse_data};
//!
//! let envelope = OutputEnvelope::new(
//!     Timestamp::parse("2000-01-01T00:00:00Z")?,
//!     OutputInput {
//!         data: Some(parse_data(r#"{"z":1,"a":2}"#)?),
//!         ..OutputInput::default()
//!     },
//! );
//! let plan = envelope.render(Indent::Four)?;
//! let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
//! plan.write_to(&mut stdout, &mut stderr)?;
//! assert_eq!(plan.exit_code, 0);
//! assert!(stderr.is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod encoding;

use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};
use thiserror::Error;

/// Serialization and input errors; no invalid input becomes a successful empty result.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OutputError {
    /// A timestamp was not in the UTC format emitted by Python's output constructor.
    #[error("timestamp must be a valid YYYY-MM-DDTHH:MM:SS[.ffffff]Z value")]
    InvalidTimestamp,
    /// Python's `FabricCLIOutput.to_json` error message and status code.
    #[error("[InvalidJson] Invalid JSON format")]
    InvalidJson,
}

/// Caller-injected UTC timestamp in Python `datetime.isoformat()` output format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Timestamp(String);

impl Timestamp {
    /// Validate a real Gregorian UTC date, with either no fraction or six digits.
    ///
    /// This never reads a clock or rewrites the caller's timestamp.
    pub fn parse(value: impl Into<String>) -> Result<Self, OutputError> {
        let value = value.into();
        let bytes = value.as_bytes();
        if !matches!(bytes.len(), 20 | 27)
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
            || bytes[16] != b':'
            || bytes[bytes.len() - 1] != b'Z'
            || (bytes.len() == 27 && bytes[19] != b'.')
            || !bytes.iter().enumerate().all(|(i, b)| {
                matches!(i, 4 | 7 | 10 | 13 | 16)
                    || i == bytes.len() - 1
                    || (bytes.len() == 27 && i == 19)
                    || b.is_ascii_digit()
            })
        {
            return Err(OutputError::InvalidTimestamp);
        }
        let number = |start: usize, end: usize| {
            bytes[start..end]
                .iter()
                .fold(0_u32, |n, b| n * 10 + u32::from(b - b'0'))
        };
        let year = number(0, 4);
        let month = number(5, 7);
        let day = number(8, 10);
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if year == 0
            || day == 0
            || day > days
            || number(11, 13) > 23
            || number(14, 16) > 59
            || number(17, 19) > 59
        {
            return Err(OutputError::InvalidTimestamp);
        }
        Ok(Self(value))
    }

    /// Borrow the exact validated timestamp.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Python's case-sensitive output status strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum OutputStatus {
    /// Successful command.
    #[default]
    Success,
    /// Failed command.
    Failure,
}

/// Homogeneous hidden-item inputs supported by Python's string/enum branches.
#[derive(Clone, Debug)]
pub enum HiddenItems {
    /// Already-resolved strings, sorted lexicographically with duplicates retained.
    Strings(Vec<String>),
    /// String-valued enum members, resolved by the caller (no runtime enum introspection).
    EnumValues(Vec<String>),
}

impl HiddenItems {
    fn sorted(self) -> Vec<String> {
        let (Self::Strings(mut items) | Self::EnumValues(mut items)) = self;
        items.sort();
        items
    }
}

/// Typed inputs to the JSON envelope; all optional values default to absent.
#[derive(Clone, Debug, Default)]
pub struct OutputInput {
    /// `None` omits the key, whereas `Some("")` retains an empty command.
    pub command: Option<String>,
    /// Success by default.
    pub status: OutputStatus,
    /// Empty strings are retained; `None` is omitted.
    pub message: Option<String>,
    /// Suppressed on success; absent or empty on failure becomes `UnexpectedError`.
    pub error_code: Option<String>,
    /// Already-projected JSON. Lists are retained, truthy non-lists are wrapped.
    pub data: Option<Value>,
    /// `Some` with an empty vector retains `hidden_data: []`.
    pub hidden_data: Option<HiddenItems>,
}

/// Python `OutputResult`'s key order and omission semantics.
#[derive(Clone, Debug, Serialize)]
pub struct OutputResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hidden_data: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl OutputResult {
    /// Borrow normalized data, distinguishing absent from an empty list.
    pub fn data(&self) -> Option<&[Value]> {
        self.data.as_deref()
    }

    /// Borrow sorted hidden values, distinguishing absent from an empty list.
    pub fn hidden_data(&self) -> Option<&[String]> {
        self.hidden_data.as_deref()
    }

    /// Borrow the effective failure code (always absent for success).
    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }

    /// Borrow the message, including an explicitly empty string.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// Product JSON envelope, independent of rendering and command dispatch.
#[derive(Clone, Debug, Serialize)]
pub struct OutputEnvelope {
    timestamp: Timestamp,
    status: OutputStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    result: OutputResult,
}

impl OutputEnvelope {
    /// Apply Python truthiness, list wrapping, hidden sorting, and error defaults.
    pub fn new(timestamp: Timestamp, input: OutputInput) -> Self {
        let data = match input.data {
            Some(Value::Array(values)) => Some(values),
            Some(value) if truthy(&value) => Some(vec![value]),
            _ => None,
        };
        let error_code = if input.status == OutputStatus::Failure {
            Some(
                input
                    .error_code
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "UnexpectedError".to_owned()),
            )
        } else {
            None
        };
        Self {
            timestamp,
            status: input.status,
            command: input.command,
            result: OutputResult {
                data,
                hidden_data: input.hidden_data.map(HiddenItems::sorted),
                error_code,
                message: input.message,
            },
        }
    }

    /// Borrow the normalized result.
    pub fn result(&self) -> &OutputResult {
        &self.result
    }

    /// Produce the semantic JSON tree, preserving object insertion order.
    pub fn to_value(&self) -> Result<Value, OutputError> {
        serde_json::to_value(self).map_err(|_| OutputError::InvalidJson)
    }

    /// Serialize without a trailing newline using Python-style JSON formatting.
    pub fn to_json(&self, indent: Indent) -> Result<String, OutputError> {
        Ok(encoding::encode(&self.to_value()?, indent))
    }

    /// Construct an inspectable stdout payload with one LF and the product exit code.
    pub fn render(&self, indent: Indent) -> Result<RenderPlan, OutputError> {
        let exit_code = match (self.status, self.result.error_code()) {
            (OutputStatus::Success, _) => 0,
            (OutputStatus::Failure, Some("Unauthorized")) => 4,
            (OutputStatus::Failure, _) => 1,
        };
        let mut payload = self.to_json(indent)?.into_bytes();
        payload.push(b'\n');
        Ok(RenderPlan {
            stream: OutputStream::Stdout,
            exit_code,
            payload,
        })
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

/// Encode JSON-domain data like Python `json.dumps(data)` with default options.
///
/// This is a data/definition encoder, **not** an output envelope. It retains
/// insertion order and ASCII escaping, uses `", "` and `": "` separators,
/// performs no list wrapping, and appends no newline. Integer `indent=0` is a
/// different pretty-printing mode; [`Indent::Zero`] deliberately does not select
/// this layout. The same i64/u64/finite-f64 domain and numeric compatibility
/// limits as the envelope serializer apply.
///
/// Use [`parse_data`] and [`finite_number`] at raw input boundaries so unsupported
/// numbers fail before an upstream serde conversion can silently discard them.
pub fn json_dumps(data: &Value) -> Result<String, OutputError> {
    Ok(encoding::encode_default(data))
}

/// Parse strict JSON instead of silently coercing malformed or non-finite data.
///
/// Unsupported Python objects, bytes, lone surrogates, and NaN/Infinity literals
/// are not JSON-domain inputs. Their source behavior is explicitly not reproduced.
/// Integers beyond i64/u64 fail instead of being silently rounded into f64.
pub fn parse_data(json: &str) -> Result<Value, OutputError> {
    let value = serde_json::from_str(json).map_err(|_| OutputError::InvalidJson)?;
    // serde_json otherwise silently rounds oversized integer tokens into f64.
    // String contents are skipped; the JSON parser above validates the syntax.
    let mut quoted = false;
    let mut escaped = false;
    for token in json.split_inclusive(|c: char| {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            return false;
        }
        if c == '"' {
            quoted = true;
        }
        c.is_ascii_whitespace() || matches!(c, ',' | ']' | '}' | ':' | '[' | '{')
    }) {
        let token = token.trim_matches(|c: char| {
            c.is_ascii_whitespace() || matches!(c, ',' | ']' | '}' | ':' | '[' | '{')
        });
        if token.starts_with(|c: char| c == '-' || c.is_ascii_digit())
            && !token.contains(['.', 'e', 'E'])
            && token.parse::<i64>().is_err()
            && token.parse::<u64>().is_err()
        {
            return Err(OutputError::InvalidJson);
        }
    }
    Ok(value)
}

/// Construct a JSON number, rejecting NaN and infinities before they can become null.
///
/// Do not use `serde_json::to_value` on unchecked floating-point values upstream:
/// serde_json itself converts non-finite floats to null before this crate sees them.
pub fn finite_number(value: f64) -> Result<Value, OutputError> {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .ok_or(OutputError::InvalidJson)
}

/// Supported Python integer-indent modes (all include newlines for nonempty containers).
#[derive(Clone, Copy, Debug, Default)]
pub enum Indent {
    /// `indent=0`, not compact JSON.
    Zero,
    /// `indent=2`.
    Two,
    /// `indent=4`, the product default.
    #[default]
    Four,
}

/// Destination stream, kept explicit instead of conflating failure with stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStream {
    /// Python JSON successes and errors both use stdout.
    Stdout,
    /// Available for callers' separate non-product diagnostics.
    Stderr,
}

/// Fully materialized output with no I/O performed during construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderPlan {
    /// Selected destination.
    pub stream: OutputStream,
    /// Caller-owned process exit status; this library never exits.
    pub exit_code: u8,
    /// UTF-8 JSON bytes followed by exactly one LF.
    pub payload: Vec<u8>,
}

impl RenderPlan {
    /// Write to the selected caller-owned stream, propagating partial-write errors.
    ///
    /// No fallback, second write, flush, or success result hides an I/O error.
    pub fn write_to(&self, stdout: &mut impl Write, stderr: &mut impl Write) -> io::Result<()> {
        match self.stream {
            OutputStream::Stdout => stdout.write_all(&self.payload),
            OutputStream::Stderr => stderr.write_all(&self.payload),
        }
    }
}

/// JSON-facing fields of Python `FabricCLIError`, not its decorated text display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    message: String,
    code: String,
}

impl CommandError {
    /// Apply Python's falsey defaults and trailing-period removal, without HTML escaping.
    ///
    /// Request IDs, verbose details and text headers are deliberately not accepted:
    /// `print_output_error` does not include them in JSON.
    pub fn new(message: Option<String>, code: Option<String>) -> Self {
        Self {
            message: message
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "An error occurred while processing the operation".to_owned())
                .trim_end_matches('.')
                .to_owned(),
            code: code
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "UnknownError".to_owned()),
        }
    }

    /// Mirror `main._handle_unexpected_error` after caller-side exception text selection.
    ///
    /// Main omits the command. If exception text extraction itself failed, the caller
    /// passes `None` to select the exact source fallback message.
    pub fn unexpected(message: Option<String>) -> Self {
        Self::new(
            Some(message.unwrap_or_else(|| "An unexpected error occurred".to_owned())),
            Some("UnexpectedError".to_owned()),
        )
    }

    /// Exact error raised by Python's envelope serializer for unsupported values.
    pub fn invalid_json() -> Self {
        Self::new(
            Some("Invalid JSON format".to_owned()),
            Some("InvalidJson".to_owned()),
        )
    }

    /// Mirror main's keyboard-interrupt JSON and exit 2, with no command field.
    ///
    /// This differs from an ordinary command error bearing the same error code,
    /// which still exits 1 through `handle_exceptions`.
    pub fn cancelled(timestamp: Timestamp, indent: Indent) -> Result<RenderPlan, OutputError> {
        let mut plan = Self::new(
            Some("Operation cancelled".to_owned()),
            Some("LongRunningOperationCancelled".to_owned()),
        )
        .envelope(timestamp, None)
        .render(indent)?;
        plan.exit_code = 2;
        Ok(plan)
    }

    /// Build the failure envelope used by Python's JSON error renderer.
    pub fn envelope(&self, timestamp: Timestamp, command: Option<String>) -> OutputEnvelope {
        OutputEnvelope::new(
            timestamp,
            OutputInput {
                status: OutputStatus::Failure,
                command,
                error_code: Some(self.code.clone()),
                message: Some(self.message.clone()),
                ..OutputInput::default()
            },
        )
    }
}
