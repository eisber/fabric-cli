// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded GET option policy for Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
//!
//! The source metadata-query branch bypasses sensitivity confirmation and
//! definition retrieval, including with `--force`. The parsed `--verbose` flag
//! is not the handlers' separate `verbose` parameter and does not change path
//! discovery depth, projection, error detail, or output presentation.

use crate::{
    errors,
    parser::{ReadCommand, ReadKind},
};
use fabric_cli_context::{Element, NodeKind};
use fabric_cli_output::CommandError;

/// Whether Python treats a nonempty query as an item metadata property.
///
/// This deliberately checks the exact root before the first dot, not a parsed
/// JMESPath expression. Whitespace, case, brackets and compound expressions are
/// not normalized into metadata queries.
pub fn is_metadata_property_query(query: &str) -> bool {
    matches!(
        query.split('.').next(),
        Some(
            "id"
                | "type"
                | "displayName"
                | "description"
                | "workspaceId"
                | "folderId"
                | "properties"
        )
    )
}

pub(crate) fn preflight(command: &ReadCommand) -> Result<(), CommandError> {
    if command.output.is_some() {
        return Err(errors::unsupported(
            "GET result-file output requires the source storage serializer and an explicit file host",
        ));
    }
    if command.kind != ReadKind::Get && (command.force || command.verbose) {
        return Err(errors::unsupported(
            "GET options cannot be used for a listing",
        ));
    }
    Ok(())
}

pub(crate) fn validate_target(command: &ReadCommand, target: &Element) -> Result<(), CommandError> {
    if !command.force && !command.verbose {
        return Ok(());
    }
    match target.kind() {
        NodeKind::Workspace(_) => Ok(()),
        NodeKind::Item(_)
            if command
                .query
                .as_deref()
                .is_some_and(is_metadata_property_query) =>
        {
            Ok(())
        }
        _ => Err(errors::unsupported(
            "GET definition options require source definition decoding and an explicit confirmation host; use a metadata-property query for bounded item reads",
        )),
    }
}
