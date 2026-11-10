// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public family mounting and host handoff, without a second executable host.

use fabric_cli_output::{
    Timestamp,
    text::{OutputFormat, ascii_width},
};
use fabric_cli_resources::{
    FamilyPresentation, FamilySurface,
    frontdoor::{AccessCommand, AccessHost, AccessOutput, register, render},
    labels_from_bytes,
};

/// Central dispatch uses its existing runtime and explicitly bound host.
pub async fn dispatch(
    host: &mut AccessHost<'_>,
    command: &AccessCommand,
    timestamp: Timestamp,
) -> Result<AccessOutput, fabric_cli_output::text::TextError> {
    let result = host.execute(command, std::future::pending()).await;
    render(
        command,
        result,
        timestamp,
        &FamilyPresentation::new(OutputFormat::Json, FamilySurface::Get),
        &ascii_width,
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = register(clap::Command::new("family-consumer")).try_get_matches_from([
        "family-consumer",
        "label",
        "list-local",
        "--output_format",
        "json",
    ])?;
    let command = AccessCommand::from_matches(&matches)?.expect("registered family");
    let result = labels_from_bytes(br#"{"labels":[{"name":"Public","id":"offline"}]}"#);
    let output = render(
        &command,
        result,
        Timestamp::parse("2000-01-01T00:00:00Z")?,
        &FamilyPresentation::new(OutputFormat::Text, FamilySurface::Get),
        &ascii_width,
    )?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr)?, 0);
    assert!(stderr.is_empty());
    assert!(String::from_utf8(stdout)?.contains("Public"));
    Ok(())
}
