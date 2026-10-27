// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Local production dispatch, shared by the binary and injected consumers.

use fabric_cli_local::{ConfigStore, LocalEffects, LocalError, LocalInvocation};
use fabric_cli_output::{
    Timestamp,
    text::{self, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_cli_resources::{FamilyPresentation, frontdoor::{self, AccessCommand}};
use std::path::Path;

/// Execute the actual local handler with explicit state, files and platform effects.
pub fn dispatch(
    command: &LocalInvocation,
    store: &ConfigStore,
    effects: &mut impl LocalEffects,
    timestamp: Timestamp,
) -> Result<TextPlan, LocalError> {
    let settings = if matches!(command.command, fabric_cli_local::LocalCommand::Version) {
        Ok(Default::default())
    } else {
        store.initialize()
    };
    let configured = settings.as_ref().ok()
        .and_then(|settings| settings.get("output_format"))
        .and_then(serde_json::Value::as_str);
    let format = OutputFormat::resolve(command.output_format.as_deref(), configured)?;
    let result = settings.and_then(|_| fabric_cli_local::execute(&command.command, store, effects));
    match result {
        Ok(output) => output.render(
            command.command.command_path(), timestamp, format,
            &text::ascii_width, Presentation::default(),
        ),
        Err(error) => Ok(text::render(
            &error.command_error().envelope(timestamp, Some(command.command.command_path().into())),
            format, &TextOptions::default(), &text::ascii_width, Presentation::default(),
        )?),
    }
}

/// Run local labels before constructing any remote credential, session or runtime.
pub fn access(
    command: &AccessCommand,
    definitions: Option<&Path>,
    timestamp: Timestamp,
    format: OutputFormat,
) -> Option<Result<frontdoor::AccessOutput, TextError>> {
    frontdoor::execute_local(command, definitions).map(|result| frontdoor::render(
        command, result, timestamp,
        &FamilyPresentation::new(format, command.surface()), &text::ascii_width,
    ))
}
