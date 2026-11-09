// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit local file acquisition and neutral structural validation dispatch.

use crate::{CliError, Control, OperationResult, check_control};
use fabric_sdk_sql_definition::{
    DefinitionLimits,
    validation::{self, v2},
};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
};

/// Caller-selected input and budgets. Discovery never reads the input file.
#[derive(Clone, Debug)]
pub struct ValidateRequest {
    /// Exact supported artifact type, not a remotely resolved target.
    pub item_type: String,
    /// Exact native format, independent of CLI output presentation.
    pub native_format: String,
    /// Explicit local rule/profile version.
    pub rule_version: String,
    /// JSON input media; no content-type guessing.
    pub media_type: String,
    /// Explicit absolute local file; directories, URLs and network shares fail.
    pub input: Option<PathBuf>,
    /// Required for checking bytes, not for schema-only discovery.
    pub limits: Option<DefinitionLimits>,
    /// Return inert registry metadata without file, config or identity access.
    pub schema_only: bool,
}

/// Discover or check explicit input through the existing neutral registration.
///
/// A completed invalid document keeps its findings in the result; the outer
/// host must return a nonzero exit independently of successful report rendering.
///
/// # Errors
/// Unsupported profile, unsafe/missing/unreadable files, budget, cancellation
/// and engine/output failures are not converted into document verdicts.
pub fn run(request: &ValidateRequest, control: &impl Control) -> Result<OperationResult, CliError> {
    check_control(control)?;
    if request.item_type != "SQLDatabase"
        || request.native_format != "dacpac"
        || request.rule_version != validation::RULE_VERSION
        || request.media_type != "application/json"
    {
        return Err(CliError::new(
            "Unsupported",
            "No exact local validation profile is registered for these selectors.",
        ));
    }
    let mut registry = v2::Registry::default();
    validation::register(&mut registry).map_err(|_| {
        CliError::new(
            "InvalidRegistration",
            "The validation operation could not be registered.",
        )
    })?;
    let target = v2::Target {
        kind: v2::TargetKind::Item,
        owner_type: request.item_type.clone(),
        facet: "src".into(),
        representation: validation::REPRESENTATION.into(),
        generation: None,
    };
    let resolution = registry
        .resolve(v2::Verb::Validate, &target, None)
        .map_err(|_| {
            CliError::new(
                "Unsupported",
                "No exact local validation binding is available.",
            )
        })?;
    if request.schema_only {
        return Ok(OperationResult::succeeded(serde_json::json!({
            "operation": resolution.operation,
            "binding": resolution.binding,
            "ruleVersion": validation::RULE_VERSION,
            "schemaDialect": validation::SCHEMA_DIALECT,
            "scope": "local_structural_subset_only",
            "targetResolution": "not_performed",
            "authorization": "not_evaluated",
            "nativeAcceptance": "not_established",
            "schemaEvaluation": "existing_typed_codec_not_general_json_schema",
        })));
    }
    let input = request.input.as_deref().ok_or_else(|| {
        CliError::new("InvalidInput", "An explicit local input file is required.")
    })?;
    let limits = request.limits.ok_or_else(|| {
        CliError::new(
            "InvalidInput",
            "Explicit finite validation budgets are required.",
        )
    })?;
    let bytes = read_input(input, limits.max_wire_bytes.get(), control)?;
    let report = validation::validate(
        &bytes,
        &request.media_type,
        &request.native_format,
        &request.rule_version,
        limits,
    )
    .map_err(|error| match error {
        validation::ValidationError::Unsupported => CliError::new("Unsupported", error.to_string()),
        validation::ValidationError::Budget(_) => {
            CliError::new("BudgetExceeded", error.to_string())
        }
    })?;
    check_control(control)?;
    serde_json::to_value(report)
        .map(OperationResult::succeeded)
        .map_err(|_| {
            CliError::new(
                "ValidationFailure",
                "The structural report could not be encoded.",
            )
        })
}

fn path_error() -> CliError {
    CliError::new(
        "UnsafePath",
        "Validation requires an absolute ordinary local file beneath nonlinked directories.",
    )
}

fn metadata(metadata: &fs::Metadata, directory: bool) -> Result<(), CliError> {
    let mut linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        linked |= metadata.file_attributes() & 0x400 != 0;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !directory {
            linked |= metadata.nlink() != 1;
        }
    }
    if linked || (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(path_error());
    }
    Ok(())
}

fn read_input(path: &Path, maximum: usize, control: &impl Control) -> Result<Vec<u8>, CliError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(path_error());
    }
    #[cfg(windows)]
    if !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
    {
        return Err(path_error());
    }
    let parent = path.parent().ok_or_else(path_error)?;
    let mut directories: Vec<_> = parent.ancestors().collect();
    directories.reverse();
    #[cfg(windows)]
    let mut held = Vec::<fs::File>::new();
    for directory in &directories {
        check_control(control)?;
        metadata(
            &fs::symlink_metadata(directory).map_err(|_| {
                CliError::new(
                    "InputReadFailed",
                    "An input directory could not be inspected.",
                )
            })?,
            true,
        )?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let handle = OpenOptions::new()
                .read(true)
                .share_mode(0x1 | 0x2)
                .custom_flags(0x02000000 | 0x00200000)
                .open(directory)
                .map_err(|_| {
                    CliError::new(
                        "InputReadFailed",
                        "An input directory could not be held safely.",
                    )
                })?;
            metadata(&handle.metadata().map_err(|_| path_error())?, true)?;
            held.push(handle);
        }
    }
    let before = fs::symlink_metadata(path)
        .map_err(|_| CliError::new("InputReadFailed", "The input file could not be inspected."))?;
    metadata(&before, false)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x1).custom_flags(0x00200000);
    }
    let mut file = options
        .open(path)
        .map_err(|_| CliError::new("InputReadFailed", "The input file could not be opened."))?;
    let opened = file.metadata().map_err(|_| path_error())?;
    metadata(&opened, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) {
            return Err(path_error());
        }
        for directory in &directories {
            metadata(
                &fs::symlink_metadata(directory).map_err(|_| path_error())?,
                true,
            )?;
        }
    }
    if opened.len() > maximum as u64 {
        return Err(CliError::new(
            "BudgetExceeded",
            "The input exceeds its explicit byte budget; no verdict was established.",
        ));
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        check_control(control)?;
        let count = file
            .read(&mut chunk)
            .map_err(|_| CliError::new("InputReadFailed", "The input file could not be read."))?;
        if count == 0 {
            break;
        }
        if count > maximum.saturating_sub(bytes.len()) {
            return Err(CliError::new(
                "BudgetExceeded",
                "The input exceeds its explicit byte budget; no verdict was established.",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    #[cfg(windows)]
    drop(held);
    check_control(control)?;
    Ok(bytes)
}
