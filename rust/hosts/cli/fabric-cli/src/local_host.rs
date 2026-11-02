// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit binary inputs for credential-free local execution.

use fabric_cli_context::{Catalog, Element, HierarchyCatalog, HomeContext, PathElement};
use fabric_cli_local::{
    AnsiTerminal, BrowserIdentity, BrowserLauncher, Capacity, ConfigStore, LocalCommand,
    LocalEffects, LocalError, LocalInvocation, NavigationHost, NavigationState,
    SystemBrowserLauncher, TerminalControl,
};
use fabric_cli_output::{
    Timestamp,
    text::{self, OutputFormat, Presentation, TextOptions, TextPlan},
};
use std::{io, path::PathBuf, process::ExitCode};

pub(crate) fn timestamp() -> Result<Timestamp, LocalError> {
    let now = azure_core::time::OffsetDateTime::now_utc();
    Ok(Timestamp::parse(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.microsecond(),
    ))?)
}

pub(crate) fn store() -> Result<ConfigStore, LocalError> {
    let root = std::env::var_os("FAB_RUST_CONFIG_ROOT")
        .filter(|root| !root.is_empty())
        .ok_or_else(|| {
            LocalError::command(
                "NotConfigured",
                "Local commands require FAB_RUST_CONFIG_ROOT",
            )
        })?;
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Err(LocalError::command(
            "InvalidInput",
            "The configuration root must be absolute",
        ));
    }
    Ok(ConfigStore::new(root))
}

pub(crate) struct Effects;

impl LocalEffects for Effects {
    fn clear_cli_cache(&mut self) -> Result<(), LocalError> {
        // The executable has command-local snapshots only. No persistent metadata
        // cache exists, and broker/config/context files must remain untouched.
        Ok(())
    }

    fn capacities(&mut self) -> Result<Vec<Capacity>, LocalError> {
        Err(LocalError::command(
            "NotConfigured",
            "Capacity selection requires the configured native metadata host",
        ))
    }

    fn browser_identity(&self) -> Result<BrowserIdentity, LocalError> {
        Err(LocalError::command(
            "NotConfigured",
            "Browser identity requires the configured native host",
        ))
    }

    fn open_browser(&mut self, url: &str) -> Result<(), LocalError> {
        SystemBrowserLauncher.open_new(url)
    }

    fn clear_terminal(&mut self) -> Result<(), LocalError> {
        AnsiTerminal::new(io::stdout().lock()).clear()
    }
}

fn execute(
    command: &LocalInvocation,
    timestamp: Timestamp,
    session: &mut Option<NavigationState>,
) -> Result<TextPlan, LocalError> {
    if matches!(command.command, LocalCommand::Version) {
        return fabric_cli::local_commands::dispatch(
            command,
            &ConfigStore::new(PathBuf::new()),
            &mut Effects,
            timestamp,
        );
    }
    let store = store()?;
    let mut initial = NavigationState::new(
        PathElement::from_fabric(Element::tenant("", "")),
        HomeContext::Unspecified,
        std::env::current_dir()?.to_string_lossy().into_owned(),
        session_id()?,
        true,
    )?;
    let navigation = matches!(
        &command.command,
        LocalCommand::ChangeDirectory { .. }
            | LocalCommand::PrintDirectory
            | LocalCommand::Open { .. }
    ) || matches!(&command.command, LocalCommand::Describe { path } if path.is_empty() || !path.join(" ").starts_with('.'));
    if !navigation {
        return fabric_cli::local_commands::dispatch(command, &store, &mut Effects, timestamp);
    }
    if session.is_none() && context_selection()?.0 != "/" {
        return Err(LocalError::command(
            "NotConfigured",
            "This context requires the configured native metadata host",
        ));
    }
    let lookup = HierarchyCatalog::new(Catalog::new(Vec::new()), Vec::new(), Vec::new());
    let restored = if session.is_none() {
        let restored = initial.restore(&store, &lookup)?;
        *session = Some(initial);
        restored
    } else {
        false
    };
    let state = session.as_mut().expect("navigation state initialized");
    let restored_path = state.current().path();
    let mut effects = Effects;
    let mut host = NavigationHost {
        state,
        lookup: &lookup,
        store: &store,
        effects: &mut effects,
    };
    let mut plan = fabric_cli::local_commands::dispatch(command, &store, &mut host, timestamp)?;
    if restored {
        let path = restored_path;
        let message = format!(
            "Command context path: {}",
            if path == "/" { "root" } else { &path }
        );
        let mut warning = text::render_notice(
            text::Notice::Warning(&message),
            None,
            Presentation::default(),
        );
        warning.records.append(&mut plan.records);
        plan.records = warning.records;
    }
    Ok(plan)
}

pub(crate) fn session_id() -> Result<u32, LocalError> {
    match std::env::var("FAB_RUST_SESSION_ID") {
        Ok(value) => value
            .parse()
            .map_err(|_| LocalError::command("InvalidInput", "Invalid explicit session ID")),
        Err(std::env::VarError::NotPresent) => Ok(std::process::id()),
        Err(_) => Err(LocalError::command(
            "InvalidInput",
            "Invalid explicit session ID",
        )),
    }
}

pub(crate) fn run(
    command: &LocalInvocation,
    session: &mut Option<NavigationState>,
) -> io::Result<ExitCode> {
    let timestamp = timestamp().map_err(io::Error::other)?;
    let plan = match execute(command, timestamp.clone(), session) {
        Ok(plan) => plan,
        Err(error) => text::render(
            &error
                .command_error()
                .envelope(timestamp, Some(command.command.command_path().into())),
            OutputFormat::Json,
            &TextOptions::default(),
            &text::ascii_width,
            Presentation::default(),
        )
        .map_err(io::Error::other)?,
    };
    plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())?;
    Ok(ExitCode::from(plan.exit_code))
}

#[cfg(feature = "native-read")]
pub(crate) fn needs_remote(command: &LocalInvocation) -> bool {
    let selected_remote = context_selection().is_ok_and(|(path, _)| path != "/");
    match &command.command {
        LocalCommand::Open { .. } => true,
        LocalCommand::ChangeDirectory { path } => path.join(" ") != "/" || selected_remote,
        LocalCommand::Describe { path } => {
            (!path.is_empty() && !path.join(" ").starts_with('.'))
                || (path.is_empty() && selected_remote)
        }
        LocalCommand::ConfigSet { key, .. } => key.eq_ignore_ascii_case("default_capacity"),
        LocalCommand::PrintDirectory => selected_remote,
        _ => false,
    }
}

pub(crate) fn context_selection() -> Result<(String, bool), LocalError> {
    match std::env::var("FAB_RUST_CONTEXT") {
        Ok(path) if path.starts_with('/') => return Ok((path, false)),
        Ok(_) | Err(std::env::VarError::NotUnicode(_)) => {
            return Err(LocalError::command(
                "InvalidInput",
                "The working context must be an absolute Fabric path",
            ));
        }
        Err(std::env::VarError::NotPresent) => {}
    }
    if std::env::var_os("FAB_RUST_CONFIG_ROOT").is_none() {
        return Ok(("/".into(), false));
    }
    let store = store()?;
    if store
        .read()?
        .get("context_persistence_enabled")
        .and_then(serde_json::Value::as_str)
        != Some("true")
    {
        return Ok(("/".into(), false));
    }
    let file = store.root().join(format!("context-{}.json", session_id()?));
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(("/".into(), false)),
        Err(error) => return Err(error.into()),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| LocalError::command("ContextLoadFailed", "Failed to load command context"))?;
    match value.get("path").and_then(serde_json::Value::as_str) {
        Some("") => Ok(("/".into(), false)),
        Some(path) if path.starts_with('/') => Ok((path.into(), true)),
        _ => Err(LocalError::command(
            "ContextLoadFailed",
            "Failed to load command context",
        )),
    }
}

pub(crate) fn local_access(
    command: &fabric_cli_resources::frontdoor::AccessCommand,
) -> io::Result<ExitCode> {
    let timestamp = timestamp().map_err(io::Error::other)?;
    let result = (|| {
        let store = store()?;
        let settings = store.initialize()?;
        let configured = settings
            .get("output_format")
            .and_then(serde_json::Value::as_str);
        let format = match command.output_format {
            Some(format) => format,
            None => OutputFormat::resolve(None, configured)?,
        };
        let definitions = settings
            .get("local_definition_labels")
            .and_then(serde_json::Value::as_str)
            .map(std::path::Path::new);
        fabric_cli::local_commands::access(command, definitions, timestamp.clone(), format)
            .expect("local-only command selected")
            .map_err(LocalError::from)
    })();
    match result {
        Ok(output) => {
            let exit = output.plan.exit_code;
            output.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())?;
            Ok(ExitCode::from(exit))
        }
        Err(error) => {
            let plan = text::render(
                &error
                    .command_error()
                    .envelope(timestamp, Some(command.command_name().into())),
                OutputFormat::Json,
                &TextOptions::default(),
                &text::ascii_width,
                Presentation::default(),
            )
            .map_err(io::Error::other)?;
            plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())?;
            Ok(ExitCode::from(plan.exit_code))
        }
    }
}
