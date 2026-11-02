// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source GET result serialization over the existing explicit export file policy.

use crate::errors;
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, OutputStream, Timestamp,
    text::{self, OutputFormat, Presentation, Style, TextOptions, TextPlan},
};
use fabric_cli_transfer::TransferFiles;
use fabric_deploy::definitions::{
    DefinitionFile, DefinitionLimits, ExportStorage, validate_part_path,
};
use serde_json::Value;
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};

/// Caller-owned filesystem and source local-clock filename provider.
///
/// The clock is captured by the executable, not discovered by the library.
/// The file policy resolves Fabric before local paths and owns all I/O.
pub struct ResultFileHost<'a> {
    /// Existing export storage capability; no ambient filesystem fallback.
    pub files: &'a mut dyn TransferFiles,
    /// Python `datetime.now().strftime("%Y%m%d_%H%M%S")`, called after projection
    /// and destination resolution, immediately before preparing the write.
    pub clock: &'a (dyn Fn() -> Result<String, CommandError> + Send + Sync),
    /// Configured presentation when the Lakehouse writer replaces Python's args.
    ///
    /// A successful Lakehouse write loses the original command and explicit
    /// format in the pinned source. `None` selects the library's JSON default.
    pub default_output_format: Option<&'a str>,
}

/// Format an explicitly supplied local time like Python's GET filename clock.
///
/// The supplied offset is retained, never converted to UTC. The executable
/// determines local time and handles offset-discovery failure; this pure helper
/// performs no clock, timezone, filesystem or service discovery.
#[cfg(feature = "native-read")]
pub fn filename_stamp(local: time::OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        local.year(),
        u8::from(local.month()),
        local.day(),
        local.hour(),
        local.minute(),
        local.second(),
    )
}

impl ResultFileHost<'_> {
    pub(crate) async fn write(
        &mut self,
        output: &str,
        name: &str,
        data: &Value,
        cancelled: impl Future<Output = ()> + Send,
    ) -> (
        Option<TextPlan>,
        Result<(), CommandError>,
        Option<ExportStorage>,
    ) {
        let mut cancelled = pin!(cancelled);
        let mut notice = None;
        let mut storage = None;
        let result = async {
            check_cancelled(cancelled.as_mut()).await?;
            let root = self
                .files
                .resolve_export_directory_async(output)
                .await
                .map_err(|error| errors::error("InvalidPath", error.to_string()))?;
            check_cancelled(cancelled.as_mut()).await?;
            let stamp = (self.clock)()?;
            if stamp.len() != 15
                || stamp.as_bytes()[8] != b'_'
                || !stamp
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| index == 8 || byte.is_ascii_digit())
            {
                return Err(errors::error("InvalidInput", "Invalid GET output clock"));
            }
            let selected_storage = self.files.export_storage(&root);
            storage = Some(selected_storage);
            let (bytes, json) = serialize(data, selected_storage)?;
            let relative = format!("{name}/get_{}{}", stamp, if json { ".json" } else { "" });
            validate_part_path(&relative, DefinitionLimits::default())
                .map_err(|_| errors::error("InvalidPath", "Invalid GET result filename"))?;
            let files = self
                .files
                .preflight_async(
                    &root,
                    vec![DefinitionFile {
                        path: relative.clone(),
                        bytes,
                    }],
                )
                .await
                .map_err(|error| errors::error("InvalidPath", error.to_string()))?;
            check_cancelled(cancelled.as_mut()).await?;
            let display = std::path::PathBuf::from(format!(
                "{}/{name}",
                self.files.export_display_path(&root, output),
            ))
            .join(format!("get_{stamp}{}", if json { ".json" } else { "" }));
            notice = Some(text::render_line(
                &format!("Exporting result to '{}'...", display.display()),
                Style::Grey,
                OutputStream::Stderr,
                Presentation::default(),
            ));
            self.files
                .create_directory_async(&root, name)
                .await
                .map_err(write_error)?;
            check_cancelled(cancelled.as_mut()).await?;
            for file in files {
                self.files
                    .write_async(&root, file)
                    .await
                    .map_err(write_error)?;
            }
            check_cancelled(cancelled.as_mut()).await
        }
        .await;
        (notice, result, storage)
    }
}

fn write_error(error: std::io::Error) -> CommandError {
    errors::error(
        "UnexpectedError",
        format!("Failed to write GET result: {error}"),
    )
}

async fn check_cancelled(
    mut cancelled: std::pin::Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<(), CommandError> {
    if poll_fn(|context| Poll::Ready(cancelled.as_mut().poll(context).is_ready())).await {
        Err(errors::error(
            "LongRunningOperationCancelled",
            "Operation cancelled; a started GET file write is not rolled back",
        ))
    } else {
        Ok(())
    }
}

/// Serialize already-projected source data, without output-envelope normalization.
///
/// Objects, arrays and JSON-valued strings acquire `.json`. Other strings and
/// scalar values use Python text spelling. Local text gets native newlines;
/// Lakehouse JSON uses Python's default ASCII/compact separators.
///
/// # Errors
/// Rejects unsupported JSON numeric domains, the released finite byte bound and
/// Lakehouse non-string scalar writes (Python fails `len(data)` after create).
pub fn serialize(data: &Value, storage: ExportStorage) -> Result<(Vec<u8>, bool), CommandError> {
    let parsed;
    let (value, json) = match data {
        Value::Array(_) | Value::Object(_) => (data, true),
        Value::String(value) => match fabric_cli_output::parse_data(value) {
            Ok(value) => {
                parsed = value;
                (&parsed, true)
            }
            Err(_) if serde_json::from_str::<Value>(value).is_ok() => {
                return Err(errors::unsupported(
                    "GET result JSON numeric domain is unsupported",
                ));
            }
            Err(_) => (data, false),
        },
        _ => (data, false),
    };
    let text = if json {
        if storage == ExportStorage::Local {
            fabric_cli_output::json_dumps_pretty_unicode(value)
        } else {
            fabric_cli_output::json_dumps(value)
                .map_err(|_| errors::unsupported("GET result JSON is unsupported"))?
        }
    } else {
        if storage == ExportStorage::Lakehouse && !value.is_string() {
            return Err(errors::unsupported(
                "Lakehouse GET result output requires JSON or string data",
            ));
        }
        text::python_string(value)
            .map_err(|_| errors::unsupported("GET result text is unsupported"))?
    };
    let text = if cfg!(windows) && storage == ExportStorage::Local {
        text.replace('\n', "\r\n")
    } else {
        text
    };
    if text.len() > DefinitionLimits::default().max_bytes.get() {
        return Err(errors::error("Limit", "GET result exceeded its byte bound"));
    }
    Ok((text.into_bytes(), json))
}

pub(crate) fn render_completion(
    result: Result<(), CommandError>,
    notice: Option<TextPlan>,
    timestamp: Timestamp,
    command: Option<&str>,
    format: OutputFormat,
) -> Result<TextPlan, text::TextError> {
    let envelope = match result {
        Ok(()) => OutputEnvelope::new(
            timestamp,
            OutputInput {
                command: command.map(str::to_owned),
                message: Some("Export completed".into()),
                ..OutputInput::default()
            },
        ),
        Err(error) => error.envelope(timestamp, command.map(str::to_owned)),
    };
    let rendered = text::render(
        &envelope,
        format,
        &TextOptions {
            subcommand: command,
            ..TextOptions::default()
        },
        &text::ascii_width,
        Presentation::default(),
    );
    let mut plan = match rendered {
        Ok(plan) => plan,
        Err(error) => {
            return Err(if let Some(notice) = notice {
                text::TextError::AfterOutput {
                    error: Box::new(error),
                    partial: Box::new(notice),
                }
            } else {
                error
            });
        }
    };
    if let Some(mut notice) = notice {
        notice.records.append(&mut plan.records);
        plan.records = notice.records;
    }
    Ok(plan)
}

/// Compose native Fabric-first output resolution over the existing file worker.
///
/// No runtime, filesystem root, credential or transport is created here.
#[cfg(feature = "native-read")]
pub fn native_files<'a, 'session>(
    local: &'a mut dyn TransferFiles,
    reads: &'a crate::session::ReadSession<'session>,
    http: &'a fabric_sdk_http::HttpExecutor,
) -> impl TransferFiles + 'a {
    crate::families::export_files::ExportFiles::new(local, reads, http)
}
