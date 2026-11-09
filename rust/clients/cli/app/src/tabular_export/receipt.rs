// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::ExportDelivery;
use fabric_cli_output::text::TextPlan;
use serde_json::{Value, json};
use std::io::{self, Write};

/// A receipt failure retaining the original execution and file-delivery outcome.
#[derive(Debug, thiserror::Error)]
#[error("Writing export receipt failed; execution and file evidence are retained")]
pub struct ReceiptWriteFailure {
    /// Original output error; never a reason to execute or publish again.
    #[source]
    pub error: io::Error,
    /// Original receipt, including the complete path and validated schema.
    pub delivery: Box<ExportDelivery>,
}

impl ReceiptWriteFailure {
    /// Preserve typed export evidence through a host's existing I/O error boundary.
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.error.kind(), self)
    }

    /// Report bounded execution/file/schema evidence, never operation or table data.
    ///
    /// At most 16 schema fields and 1,024 path bytes are included. Truncation is
    /// explicit; the typed error still retains the complete original receipt.
    pub fn report(&self, stderr: &mut impl Write) -> io::Result<()> {
        const MAX_BYTES: usize = 16 * 1024;
        let receipt = &self.delivery.receipt;
        let delivery = &receipt["delivery"];
        let (source, _) = bounded_text(&receipt["source"], 128);
        let (execution, _) = bounded_text(&receipt["execution"]["status"], 32);
        let (status, _) = bounded_text(&delivery["status"], 32);
        let (file, file_truncated) = bounded_text(&delivery["file"], 1024);
        let (format, _) = bounded_text(&delivery["format"], 16);
        let columns = delivery["schema"].as_array();
        let total_columns = columns.map_or(0, Vec::len);
        let mut schema_truncated = total_columns > 16;
        let schema = columns
            .into_iter()
            .flatten()
            .take(16)
            .map(|column| {
                let (name, truncated) = bounded_text(&column["name"], 64);
                let (kind, _) = bounded_text(&column["type"], 16);
                schema_truncated |= truncated;
                json!({"name": name, "type": kind, "nullable": column["nullable"].as_bool()})
            })
            .collect::<Vec<_>>();
        let mut diagnostic = json!({
            "error": {
                "code": "export_receipt_write_failed",
                "message": "Receipt output failed. Execution and file delivery were not replayed."
            },
            "retained_export": {
                "source": source,
                "execution": {"status": execution},
                "delivery": {
                    "status": status, "file": file, "file_truncated": file_truncated,
                    "format": format, "rows": delivery["rows"].as_u64(),
                    "bytes": delivery["bytes"].as_u64(),
                    "schema": schema, "schema_columns": total_columns,
                    "schema_truncated": schema_truncated
                }
            }
        });
        let mut bytes = serde_json::to_vec(&diagnostic).map_err(io::Error::other)?;
        if bytes.len() + 1 > MAX_BYTES {
            diagnostic["retained_export"]["delivery"]["schema"] = json!([]);
            diagnostic["retained_export"]["delivery"]["schema_truncated"] = json!(true);
            bytes = serde_json::to_vec(&diagnostic).map_err(io::Error::other)?;
        }
        if bytes.len() + 1 > MAX_BYTES {
            return Err(io::Error::other(
                "Export receipt diagnostic exceeded its bound",
            ));
        }
        bytes.push(b'\n');
        stderr.write_all(&bytes)?;
        stderr.flush()
    }
}

impl ExportDelivery {
    /// Write and flush one receipt, retaining effects and reporting failure to stderr.
    pub fn write_to_streams(
        self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<u8, ReceiptWriteFailure> {
        self.write_with_prefix(
            &TextPlan {
                records: Vec::new(),
                exit_code: 0,
            },
            stdout,
            stderr,
        )
    }

    /// Write native notices and the receipt once, retaining evidence on any I/O failure.
    pub fn write_with_prefix(
        self,
        prefix: &TextPlan,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<u8, ReceiptWriteFailure> {
        let result = prefix
            .write_to(stdout, stderr)
            .and_then(|_| self.write_to(stdout))
            .and_then(|code| stdout.flush().map(|()| code));
        match result {
            Ok(code) => Ok(code),
            Err(error) => {
                let failure = ReceiptWriteFailure {
                    error,
                    delivery: Box::new(self),
                };
                let _ = failure.report(stderr);
                Err(failure)
            }
        }
    }
}

fn bounded_text(value: &Value, limit: usize) -> (Value, bool) {
    let Some(text) = value.as_str() else {
        return (Value::Null, false);
    };
    let end = text
        .char_indices()
        .map(|(offset, character)| offset + character.len_utf8())
        .take_while(|end| *end <= limit)
        .last()
        .unwrap_or(0);
    (json!(&text[..end]), end < text.len())
}
