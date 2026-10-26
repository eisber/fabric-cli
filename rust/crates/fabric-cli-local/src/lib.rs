// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral local CLI family translated from Python
//! `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
//!
//! The caller mounts [`root_commands`], converts with [`from_matches`], and
//! executes with an explicit [`ConfigStore`] and [`LocalEffects`]. No user
//! profile, credential, service, browser, terminal or runtime is discovered.

mod config;
mod parser;

use std::io;

use fabric_cli_output::text::{
    self, CharacterWidth, OutputFormat, Presentation, TextOptions, TextPlan,
};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputError, OutputInput, OutputStream, Timestamp,
};
use serde_json::{Value, json};
use thiserror::Error;

pub use config::{ConfigStore, SETTINGS, Setting};
pub use parser::{from_matches, root_commands};

/// Python product source version, not the native preview package version.
pub const SOURCE_PRODUCT_VERSION: &str = "1.8.0";
/// Native preview package version, deliberately distinguished from Python.
pub const PREVIEW_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Fully typed implemented local commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalCommand {
    /// Validate and persist a setting.
    ConfigSet {
        /// Case-insensitive source key.
        key: String,
        /// Source value before whitespace and edge-quote stripping.
        value: String,
    },
    /// Read a setting, printing nothing for absent/falsey values.
    ConfigGet {
        /// Case-insensitive source key.
        key: String,
    },
    /// List all eighteen source settings in their registered order.
    ConfigList,
    /// Invalidate only the injected CLI metadata caches.
    ClearCache,
    /// Print the native preview version through the named Python leaf.
    Version,
}

/// One local invocation and its source-global presentation options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalInvocation {
    /// Typed operation; execution remains separate from parsing.
    pub command: LocalCommand,
    /// Explicit output override, otherwise selected by the host from config.
    pub output_format: Option<String>,
    /// Accepted source skill selection; local operations send no HTTP headers.
    pub skill: Option<String>,
}

impl LocalCommand {
    /// Canonical decorated command path for the existing output pipeline.
    pub const fn command_path(&self) -> &'static str {
        match self {
            Self::ConfigSet { .. } => "config set",
            Self::ConfigGet { .. } => "config get",
            Self::ConfigList => "config ls",
            Self::ClearCache => "config clear-cache",
            Self::Version => "version",
        }
    }
}

/// Non-secret capacity metadata already fetched by the caller's existing SDK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capacity {
    /// Source `displayName`.
    pub display_name: String,
    /// Source capacity identifier.
    pub id: String,
}

/// Explicit side effects supplied by the owning command host.
pub trait LocalEffects {
    /// Clear CLI metadata caches, never identity/broker/token stores.
    fn clear_cli_cache(&mut self) -> Result<(), LocalError>;

    /// Obtain the caller's capacity snapshot for source display-name validation.
    ///
    /// Implementations must not block an async runtime to perform service I/O.
    /// An async host may fetch this snapshot before calling [`execute`].
    fn capacities(&mut self) -> Result<Vec<Capacity>, LocalError>;
}

/// Structured failures returned to the central host; this crate never exits.
#[derive(Debug, Error)]
pub enum LocalError {
    /// Source command error with its product code.
    #[error("{message}")]
    Command {
        /// Product code.
        code: &'static str,
        /// Human-readable source error.
        message: String,
    },
    /// Explicit malformed settings instead of silent destructive defaults.
    #[error("The CLI configuration must contain a valid JSON object")]
    InvalidConfig,
    /// File, writer, or injected platform effect failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Shared JSON serialization failed.
    #[error(transparent)]
    Json(#[from] OutputError),
    /// Shared text rendering failed.
    #[error(transparent)]
    Text(#[from] text::TextError),
}

impl LocalError {
    /// Construct a source-coded error without printing or terminating.
    pub fn command(code: &'static str, message: impl Into<String>) -> Self {
        Self::Command {
            code,
            message: message.into(),
        }
    }

    /// Adapt to the existing shared output error pipeline.
    pub fn command_error(&self) -> CommandError {
        let code = match self {
            Self::Command { code, .. } => *code,
            Self::InvalidConfig => "InvalidJson",
            Self::Io(_) => "LocalIOError",
            Self::Json(_) => "InvalidJson",
            Self::Text(error) => return error.command_error(),
        };
        CommandError::new(Some(self.to_string()), Some(code.to_owned()))
    }
}

/// Source result plus explicit pre-output notices, not process-global writers.
#[derive(Clone, Debug)]
pub struct LocalOutput {
    input: Option<OutputInput>,
    headers: bool,
    prelude: Vec<LocalNotice>,
}

#[derive(Clone, Debug)]
enum LocalNotice {
    Grey(String),
    Warning(&'static str),
    Version,
}

impl LocalOutput {
    fn data(data: Value) -> Self {
        Self {
            input: Some(OutputInput {
                data: Some(data),
                ..OutputInput::default()
            }),
            headers: false,
            prelude: Vec::new(),
        }
    }

    fn message(message: String) -> Self {
        Self {
            input: Some(OutputInput {
                message: Some(message),
                ..OutputInput::default()
            }),
            headers: false,
            prelude: Vec::new(),
        }
    }

    fn empty() -> Self {
        Self {
            input: None,
            headers: false,
            prelude: Vec::new(),
        }
    }

    /// Render through the existing output pipeline using explicit caller inputs.
    ///
    /// An absent `config get` stays genuinely silent in both formats.
    pub fn render(
        &self,
        command: &str,
        timestamp: Timestamp,
        format: OutputFormat,
        widths: &impl CharacterWidth,
        presentation: Presentation,
    ) -> Result<TextPlan, LocalError> {
        let mut plan = TextPlan {
            records: Vec::new(),
            exit_code: 0,
        };
        for notice in &self.prelude {
            let rendered = match notice {
                LocalNotice::Grey(message) => text::render_line(
                    message,
                    text::Style::Grey,
                    OutputStream::Stderr,
                    presentation,
                ),
                LocalNotice::Warning(message) => {
                    text::render_notice(text::Notice::Warning(message), None, presentation)
                }
                LocalNotice::Version => text::render_version(PREVIEW_VERSION, presentation),
            };
            plan.records.extend(rendered.records);
        }
        if let Some(input) = &self.input {
            let mut input = input.clone();
            input.command = Some(command.to_owned());
            let rendered = text::render(
                &OutputEnvelope::new(timestamp, input),
                format,
                &TextOptions {
                    show_headers: self.headers,
                    ..TextOptions::default()
                },
                widths,
                presentation,
            )?;
            plan.exit_code = rendered.exit_code;
            plan.records.extend(rendered.records);
        }
        Ok(plan)
    }
}

/// Execute a real family handler against explicit files and host-owned effects.
///
/// Capacity metadata is supplied by the host; no service or credential fallback
/// is installed. REPL entry through deprecated `config set mode interactive`
/// fails explicitly because this slice does not implement a REPL.
pub fn execute(
    command: &LocalCommand,
    store: &ConfigStore,
    effects: &mut impl LocalEffects,
) -> Result<LocalOutput, LocalError> {
    if matches!(command, LocalCommand::Version) {
        return Ok(LocalOutput {
            input: None,
            headers: false,
            prelude: vec![LocalNotice::Version],
        });
    }
    let mut settings = store.initialize()?;
    match command {
        LocalCommand::ConfigSet { key, value } => {
            let key = key.to_lowercase();
            let value = value.trim().trim_matches('\'').trim_matches('"');
            if key == "mode" {
                if value == "interactive" {
                    return Err(LocalError::command(
                        "Unsupported",
                        "Interactive mode is not implemented in the native preview",
                    ));
                }
                return Ok(mode_output(false));
            }
            let setting = config::setting(&key)?;
            if !setting.allowed.is_empty() && !setting.allowed.contains(&value) {
                let allowed = setting
                    .allowed
                    .iter()
                    .map(|value| format!("'{value}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(LocalError::command(
                    "InvalidInput",
                    format!(
                        "'{value}' is not a valid value for '{key}'. Allowed values are: [{allowed}]"
                    ),
                ));
            }
            let mut capacity_id = None;
            let value = if key == "default_capacity" {
                let name = value.replace(".Capacity", "").replace(".capacity", "");
                if let Some(capacity) = effects
                    .capacities()?
                    .into_iter()
                    .find(|capacity| capacity.display_name.to_lowercase() == name.to_lowercase())
                {
                    capacity_id = Some(capacity.id);
                    name
                } else {
                    return Err(LocalError::command(
                        "InvalidInput",
                        format!(
                            "'{name}' is not a valid capacity. Please provide a valid capacity name"
                        ),
                    ));
                }
            } else {
                value.to_owned()
            };
            if key == "local_definition_labels" && !std::path::Path::new(&value).try_exists()? {
                return Err(LocalError::command(
                    "InvalidPath",
                    "The specified file or directory does not exist",
                ));
            }
            settings.insert(key.clone(), Value::String(value.to_owned()));
            if let Some(id) = capacity_id {
                settings.insert("fab_default_capacity_id".to_owned(), Value::String(id));
            }
            store.write(&settings)?;
            let mut result =
                LocalOutput::message(format!("Configuration '{key}' set to '{value}'"));
            result
                .prelude
                .push(LocalNotice::Grey(format!("Updating '{key}' value...")));
            Ok(result)
        }
        LocalCommand::ConfigGet { key } => {
            let key = key.to_lowercase();
            if key == "mode" {
                return Ok(mode_output(true));
            }
            config::setting(&key)?;
            Ok(match settings.get(&key).filter(|value| truthy(value)) {
                Some(value) => LocalOutput::data(value.clone()),
                None => LocalOutput::empty(),
            })
        }
        LocalCommand::ConfigList => {
            let rows = SETTINGS.iter().map(|setting| json!({
                "setting": setting.key,
                "value": settings.get(setting.key).cloned().unwrap_or_else(|| Value::String(String::new())),
            })).collect::<Vec<_>>();
            let mut result = LocalOutput::data(Value::Array(rows));
            result.headers = true;
            Ok(result)
        }
        LocalCommand::ClearCache => {
            effects.clear_cli_cache()?;
            Ok(LocalOutput::message("Cleared the cache".to_owned()))
        }
        LocalCommand::Version => unreachable!("version returned before configuration I/O"),
    }
}

fn mode_output(get: bool) -> LocalOutput {
    let mut result = if get {
        LocalOutput::data(Value::String("command_line".to_owned()))
    } else {
        LocalOutput::empty()
    };
    result.prelude.push(LocalNotice::Warning("The 'mode' setting is deprecated. Run 'fab' without arguments to enter REPL mode, or use 'fab <command>' for command-line mode."));
    result
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
