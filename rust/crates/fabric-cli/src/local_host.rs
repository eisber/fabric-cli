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
        now.year(), u8::from(now.month()), now.day(), now.hour(), now.minute(),
        now.second(), now.microsecond(),
    ))?)
}

pub(crate) fn store() -> Result<ConfigStore, LocalError> {
    let root = std::env::var_os("FAB_RUST_CONFIG_ROOT")
        .filter(|root| !root.is_empty())
        .ok_or_else(|| LocalError::command("NotConfigured", "Local commands require FAB_RUST_CONFIG_ROOT"))?;
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Err(LocalError::command("InvalidInput", "The configuration root must be absolute"));
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
        Err(LocalError::command("NotConfigured", "Capacity selection requires the configured native metadata host"))
    }

    fn browser_identity(&self) -> Result<BrowserIdentity, LocalError> {
        Err(LocalError::command("NotConfigured", "Browser identity requires the configured native host"))
    }

    fn open_browser(&mut self, url: &str) -> Result<(), LocalError> {
        SystemBrowserLauncher.open_new(url)
    }

    fn clear_terminal(&mut self) -> Result<(), LocalError> {
        AnsiTerminal::new(io::stdout().lock()).clear()
    }
}

fn execute(command: &LocalInvocation, timestamp: Timestamp) -> Result<TextPlan, LocalError> {
    let store = if matches!(command.command, LocalCommand::Version) {
        ConfigStore::new(std::env::current_dir()?)
    } else {
        store()?
    };
    let tenant = Element::tenant("", "");
    let lookup = HierarchyCatalog::new(Catalog::new(Vec::new()), Vec::new(), Vec::new());
    let mut state = NavigationState::new(
        PathElement::from_fabric(tenant), HomeContext::Unspecified,
        std::env::current_dir()?.to_string_lossy().into_owned(), session_id()?, true,
    )?;
    if !matches!(command.command, LocalCommand::Version) {
        state.restore(&store, &lookup)?;
    }
    let mut effects = Effects;
    let mut host = NavigationHost { state: &mut state, lookup: &lookup, store: &store, effects: &mut effects };
    fabric_cli::local_commands::dispatch(command, &store, &mut host, timestamp)
}

fn session_id() -> Result<u32, LocalError> {
    match std::env::var("FAB_RUST_SESSION_ID") {
        Ok(value) => value.parse().map_err(|_| LocalError::command("InvalidInput", "Invalid explicit session ID")),
        Err(std::env::VarError::NotPresent) => Ok(std::process::id()),
        Err(_) => Err(LocalError::command("InvalidInput", "Invalid explicit session ID")),
    }
}

pub(crate) fn run(command: &LocalInvocation) -> io::Result<ExitCode> {
    let timestamp = timestamp().map_err(io::Error::other)?;
    let plan = match execute(command, timestamp.clone()) {
        Ok(plan) => plan,
        Err(error) => text::render(
            &error.command_error().envelope(timestamp, Some(command.command.command_path().into())),
            OutputFormat::Json, &TextOptions::default(), &text::ascii_width, Presentation::default(),
        ).map_err(io::Error::other)?,
    };
    plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())?;
    Ok(ExitCode::from(plan.exit_code))
}

pub(crate) fn local_access(
    command: &fabric_cli_resources::frontdoor::AccessCommand,
) -> io::Result<ExitCode> {
    let timestamp = timestamp().map_err(io::Error::other)?;
    let result = (|| {
        let store = store()?;
        let settings = store.initialize()?;
        let configured = settings.get("output_format").and_then(serde_json::Value::as_str);
        let format = command.output_format.unwrap_or(OutputFormat::resolve(None, configured)?);
        let definitions = settings.get("local_definition_labels").and_then(serde_json::Value::as_str).map(std::path::Path::new);
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
                &error.command_error().envelope(timestamp, Some(command.command_name().into())),
                OutputFormat::Json, &TextOptions::default(), &text::ascii_width, Presentation::default(),
            ).map_err(io::Error::other)?;
            plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())?;
            Ok(ExitCode::from(plan.exit_code))
        }
    }
}
