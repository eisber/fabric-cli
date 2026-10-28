// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral local CLI family translated from Python
//! `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
//!
//! The caller mounts [`root_commands`], converts with [`from_matches`], and
//! executes with an explicit [`ConfigStore`] and [`LocalEffects`]. No user
//! profile, credential, service, browser, terminal or runtime is discovered.

mod config;
mod navigation;
mod parser;
mod platform;
mod support;

use std::io;
use std::sync::Arc;

use fabric_cli_context::{
    ContextError, NodeKind, PathElement, PathFailure, PathKind, PathResolution,
};
use fabric_cli_output::text::{
    self, CharacterWidth, OutputFormat, Presentation, TextOptions, TextPlan,
};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputError, OutputInput, OutputStream, Timestamp,
};
use serde_json::{Value, json};
use thiserror::Error;

pub use config::{ConfigStore, SETTINGS, Setting};
pub use navigation::{NavigationHost, NavigationState};
pub use parser::{from_matches, root_commands};
pub use platform::{AnsiTerminal, BrowserLauncher, SystemBrowserLauncher, TerminalControl};
pub use support::{SupportEntry, SupportMatrix};

/// Python product source version, not the native preview package version.
pub const SOURCE_PRODUCT_VERSION: &str = "1.8.0";
/// Native preview package version, deliberately distinguished from Python.
pub const PREVIEW_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Fully typed implemented local commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalCommand {
    /// Display source configuration-group help; not another canonical leaf.
    ConfigHelp,
    /// Resolve and switch the caller's actual session context.
    ChangeDirectory {
        /// Source path tokens, joined by the existing context resolver.
        path: Vec<String>,
    },
    /// Print source current-context metadata and injected local working directory.
    PrintDirectory,
    /// Select a source browser URL and invoke the explicit launcher.
    Open {
        /// Source workspace or item path tokens.
        path: Vec<String>,
    },
    /// Clear the explicitly injected terminal.
    Clear,
    /// Describe source command support for a real path or recognized dot suffix.
    Describe {
        /// Optional source path tokens; empty selects the current context.
        path: Vec<String>,
    },
    /// Preserve Python's unsupported-feature warning, not a plugin loader.
    Extension,
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
    /// Python's `args.command` for successful output, retaining grouped config names.
    pub const fn command_name(&self) -> &'static str {
        match self {
            Self::ConfigHelp
            | Self::ConfigSet { .. }
            | Self::ConfigGet { .. }
            | Self::ConfigList
            | Self::ClearCache => "config",
            _ => self.command_path(),
        }
    }

    /// Canonical decorated command path for the existing output pipeline.
    pub const fn command_path(&self) -> &'static str {
        match self {
            Self::ConfigHelp => "config",
            Self::ChangeDirectory { .. } => "cd",
            Self::PrintDirectory => "pwd",
            Self::Open { .. } => "open",
            Self::Clear => "clear",
            Self::Describe { .. } => "desc",
            Self::Extension => "extension",
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

/// Caller-supplied non-secret identity metadata for source browser routing.
///
/// Never provide a token. Claim order is preserved; `tid` becomes the final
/// `ctid` browser parameter exactly as in the pinned source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowserIdentity {
    /// The existing authenticated host confirms a user, not service-principal, identity.
    pub user: bool,
    /// Explicit `upn` and `tid` claims only, already selected by the host.
    pub claims: Vec<(String, String)>,
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

    /// Current real context; [`NavigationHost`] supplies the standard implementation.
    fn current_context(&self) -> Result<Arc<PathElement>, LocalError> {
        Err(not_configured("current context"))
    }

    /// Explicit OS working directory, never inferred inside the family.
    fn local_directory(&self) -> Result<String, LocalError> {
        Err(not_configured("local working directory"))
    }

    /// Existing context resolution over caller-owned metadata, never a new service stack.
    fn resolve_path(&mut self, _path: &[String]) -> Result<PathResolution, LocalError> {
        Err(not_configured("path resolver"))
    }

    /// Actually commit session state and, when selected, persist its context path.
    fn change_context(
        &mut self,
        _context: Arc<PathElement>,
        _persistence: bool,
    ) -> Result<(), LocalError> {
        Err(not_configured("mutable session context"))
    }

    /// Non-secret metadata from the explicitly selected existing identity host.
    fn browser_identity(&self) -> Result<BrowserIdentity, LocalError> {
        Err(not_configured("browser identity metadata"))
    }

    /// Invoke an explicitly configured browser launcher; offline consumers capture it.
    fn open_browser(&mut self, _url: &str) -> Result<(), LocalError> {
        Err(not_configured("browser launcher"))
    }

    /// Clear the explicitly configured terminal, never spawn a shell command string.
    fn clear_terminal(&mut self) -> Result<(), LocalError> {
        Err(not_configured("terminal control"))
    }
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
    /// Existing context algorithm error, retaining the source product code.
    #[error(transparent)]
    Context(#[from] ContextError),
    /// Resolution failure retains the shared resolver's pre-error notices.
    #[error(transparent)]
    Path(PathFailure),
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
            Self::Context(error) => {
                return CommandError::new(Some(error.message.clone()), Some(error.code.clone()));
            }
            Self::Path(failure) => {
                return CommandError::new(
                    Some(failure.error.message.clone()),
                    Some(failure.error.code.clone()),
                );
            }
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
    exit_code: u8,
}

#[derive(Clone, Debug)]
enum LocalNotice {
    Grey(String),
    Warning(&'static str),
    CommandWarning(&'static str, &'static str),
    Plain(String),
    Version,
    ConfigHelp,
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
            exit_code: 0,
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
            exit_code: 0,
        }
    }

    fn empty() -> Self {
        Self {
            input: None,
            headers: false,
            prelude: Vec::new(),
            exit_code: 0,
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
            exit_code: self.exit_code,
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
                LocalNotice::ConfigHelp => text::render_help(
                    &[text::HelpCategory {
                        name: "Commands",
                        commands: &[
                            ("clear-cache", "Clear the CLI cache."),
                            ("get", "Get the value of a configuration key."),
                            ("ls", "List all configuration keys and their values."),
                            ("set", "Set a configuration key to a specified value."),
                        ],
                    }],
                    Some("Manage CLI configuration."),
                    presentation,
                )?,
                LocalNotice::CommandWarning(message, command) => {
                    text::render_notice(text::Notice::Warning(message), Some(command), presentation)
                }
                LocalNotice::Plain(message) => text::render_line(
                    message,
                    text::Style::Plain,
                    OutputStream::Stdout,
                    presentation,
                ),
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
            exit_code: 0,
        });
    }
    let mut settings = store.initialize()?;
    match command {
        LocalCommand::ConfigHelp => {
            let mut output = LocalOutput::empty();
            output.prelude.push(LocalNotice::ConfigHelp);
            Ok(output)
        }
        LocalCommand::ChangeDirectory { path } => {
            let resolution = effects.resolve_path(path)?;
            SupportMatrix::source()?.check(&resolution.element, "cd")?;
            let message = if resolution.element.kind() == PathKind::Fabric(NodeKind::Tenant) {
                "Switched to root".to_owned()
            } else {
                format!("Switched to '{}'", resolution.element.name())
            };
            let persistence = settings
                .get("context_persistence_enabled")
                .and_then(Value::as_str)
                == Some("true");
            effects.change_context(resolution.element, persistence)?;
            let mut output = LocalOutput::message(message);
            add_path_notices(&mut output, resolution.notices);
            Ok(output)
        }
        LocalCommand::PrintDirectory => {
            let context = effects.current_context()?;
            let full_name = context.name();
            let mut parts = full_name.split('.');
            let name = parts.next().unwrap_or("");
            let suffix = parts
                .next()
                .map(|value| format!(".{value}"))
                .unwrap_or_default();
            let data = json!({
                "name": name, "type": suffix, "id": context.id().unwrap_or(""),
                "local_path": effects.local_directory()?,
            });
            let mut output = LocalOutput::empty();
            output
                .prelude
                .push(LocalNotice::Grey(config::pretty_json(&data, 0)?));
            Ok(output)
        }
        LocalCommand::Open { path } => open(path, &settings, effects),
        LocalCommand::Clear => {
            effects.clear_terminal()?;
            Ok(LocalOutput::empty())
        }
        LocalCommand::Describe { path } => describe(path, effects),
        LocalCommand::Extension => {
            let mut output = LocalOutput::empty();
            output.prelude.push(LocalNotice::CommandWarning(
                "Feature is not supported",
                "extension",
            ));
            Ok(output)
        }
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

fn not_configured(boundary: &str) -> LocalError {
    LocalError::command(
        "NotConfigured",
        format!("The local command host has no {boundary}"),
    )
}

fn add_path_notices(output: &mut LocalOutput, notices: Vec<fabric_cli_context::PathNotice>) {
    for notice in notices {
        match notice {
            fabric_cli_context::PathNotice::ExternalDataSharing => output.prelude.push(
                LocalNotice::Warning("Ensure tenant setting is enabled for External data sharing"),
            ),
            fabric_cli_context::PathNotice::LocalFallback(message) => {
                output.prelude.push(LocalNotice::Grey(message))
            }
        }
    }
}

fn open(
    path: &[String],
    settings: &serde_json::Map<String, Value>,
    effects: &mut impl LocalEffects,
) -> Result<LocalOutput, LocalError> {
    let resolution = effects.resolve_path(path)?;
    let matrix = SupportMatrix::source()?;
    let element = &resolution.element;
    matrix.check(element, "open")?;
    let experience = if settings
        .get("default_open_experience")
        .and_then(Value::as_str)
        == Some("powerbi")
    {
        "power-bi"
    } else {
        "fabric-developer"
    };
    let Some(id) = element.id().filter(|id| !id.is_empty()) else {
        return Ok(LocalOutput::empty());
    };
    let url = match element.kind() {
        PathKind::Fabric(NodeKind::Workspace(_)) => {
            format!("https://app.fabric.microsoft.com/groups/{id}/list?experience={experience}")
        }
        PathKind::Fabric(NodeKind::Item(kind)) => {
            let mut parent = element.parent();
            let workspace = loop {
                match parent {
                    Some(node)
                        if matches!(node.kind(), PathKind::Fabric(NodeKind::Workspace(_))) =>
                    {
                        break node;
                    }
                    Some(node) => parent = node.parent(),
                    None => {
                        return Err(LocalError::command(
                            "InvalidContext",
                            "The item has no workspace",
                        ));
                    }
                }
            };
            let Some(workspace_id) = workspace.id().filter(|id| !id.is_empty()) else {
                return Ok(LocalOutput::empty());
            };
            let route = matrix
                .uris
                .get(kind.as_str())
                .map(String::as_str)
                .unwrap_or("");
            format!(
                "https://app.fabric.microsoft.com/groups/{workspace_id}/{route}/{id}/?experience={experience}"
            )
        }
        _ => {
            return Err(LocalError::command(
                "UnsupportedCommand",
                "not supported for command 'open'",
            ));
        }
    };
    let identity = effects.browser_identity()?;
    if !identity.user {
        return Err(LocalError::command(
            "AuthenticationFailed",
            "This operation is only supported with user authentication",
        ));
    }
    let mut launch_url = url.clone();
    for (key, value) in &identity.claims {
        if !matches!(key.as_str(), "upn" | "tid") {
            return Err(LocalError::command(
                "InvalidInput",
                "Only explicit upn and tid browser claims are accepted",
            ));
        }
        if key == "upn" {
            launch_url.push_str(&format!("&upn={value}"));
        }
    }
    for (key, value) in &identity.claims {
        if key == "tid" {
            launch_url.push_str(&format!("&ctid={value}"));
        }
    }
    effects.open_browser(&launch_url)?;
    let mut output = LocalOutput::message(url);
    add_path_notices(&mut output, resolution.notices);
    output.prelude.push(LocalNotice::Grey(format!(
        "Opening '{}' in the web browser...",
        element.name()
    )));
    Ok(output)
}

fn describe(path: &[String], effects: &mut impl LocalEffects) -> Result<LocalOutput, LocalError> {
    let matrix = SupportMatrix::source()?;
    let input = path.join(" ").trim_matches(['\'', '"']).to_owned();
    let mut output = LocalOutput::empty();
    if let Some(suffix) = matrix.canonical_suffix(&input) {
        let element = matrix.model(suffix)?;
        output
            .prelude
            .push(LocalNotice::Plain(matrix.describe(suffix, &element)));
        return Ok(output);
    }
    // The source's "all" help tip does not implement a special all-elements mode.
    // Preserve its invalid-path usage contract rather than inventing an expansion.
    if input.eq_ignore_ascii_case("all") {
        let mut elements = matrix.elements.clone();
        elements.sort();
        output.exit_code = 2;
        output.prelude.push(LocalNotice::Warning(
            "unknown Fabric element or valid path for \"desc\"\n",
        ));
        output.prelude.push(LocalNotice::Plain(format!(
            "Usage:  desc <fabric_dot_element> or <valid_path>\n\nAvailable elements:\n  {}\n",
            elements.join("\n  "),
        )));
        return Ok(output);
    }
    let resolution = effects.resolve_path(path)?;
    add_path_notices(&mut output, resolution.notices);
    output.prelude.push(LocalNotice::Plain(
        matrix.describe(&resolution.element.name(), &resolution.element),
    ));
    Ok(output)
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
