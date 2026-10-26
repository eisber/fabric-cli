// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline consumer: takes an explicitly disposable config directory argument.

use fabric_cli_local::{Capacity, ConfigStore, LocalCommand, LocalEffects, LocalError, execute};
use fabric_cli_output::Timestamp;
use fabric_cli_output::text::{OutputFormat, Presentation, ascii_width};

struct Host;

impl LocalEffects for Host {
    fn clear_cli_cache(&mut self) -> Result<(), LocalError> {
        Err(LocalError::command(
            "NotConfigured",
            "This example has no metadata cache",
        ))
    }

    fn capacities(&mut self) -> Result<Vec<Capacity>, LocalError> {
        Err(LocalError::command(
            "NotConfigured",
            "This example has no capacity snapshot",
        ))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("Pass an explicitly disposable config directory")?;
    let store = ConfigStore::new(root);
    execute(
        &LocalCommand::ConfigSet {
            key: "debug_enabled".to_owned(),
            value: "true".to_owned(),
        },
        &store,
        &mut Host,
    )?;
    let result = execute(
        &LocalCommand::ConfigGet {
            key: "debug_enabled".to_owned(),
        },
        &store,
        &mut Host,
    )?;
    let plan = result.render(
        "config get",
        Timestamp::parse("2000-01-01T00:00:00Z")?,
        OutputFormat::Json,
        &ascii_width,
        Presentation::default(),
    )?;
    plan.write_to(&mut std::io::stdout(), &mut std::io::stderr())?;
    Ok(())
}
