// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin v2 metadata Exists arguments/delivery over the shared SDK operation.
//! Parsing validates the whole supported target before host/credential access.

use crate::get_target::{GetRepresentation, GetTarget, is_id};
use clap::{Arg, Command, error::ErrorKind};
use fabric_sdk::{
    api::v2_outcome::{RetentionError, RetentionLimits},
    exists::{
        ExistsError, ExistsLimits, ExistsReport, ExistsRequest, ExistsTarget, MetadataSelector,
    },
};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    num::NonZeroUsize,
    time::Duration,
};

/// Validated metadata-only invocation, with no representation/local fallback.
#[derive(Clone, Debug)]
pub struct ExistsCommand {
    /// Typed SDK target, shared by every execution head.
    pub target: ExistsTarget,
    /// Explicit report/source-byte budget.
    pub max_bytes: NonZeroUsize,
    /// One total caller deadline, including resolution and observation.
    pub timeout: Duration,
}

/// Sanitized parser/capability error; supplied input is never echoed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExistsArgumentError {
    /// Clap's help or syntax classification.
    #[error("invalid Exists arguments; run exists --help")]
    Syntax(ErrorKind),
    /// No metadata target binding exists for this spelling.
    #[error(
        "Exists requires an absolute metadata target or ID-bound Lakehouse catalog selector; physical paths are deferred"
    )]
    Target,
}

/// Build only the owned metadata Exists schema, preserving v1 grammar.
pub fn exists_command() -> Command {
    Command::new("exists")
        .infer_long_args(false)
        .args_override_self(false)
        .about("Observe scoped metadata existence; unknown/denied never become false")
        .after_help("Workspace/root-item metadata and ID-bound Lakehouse catalog only. Catalog: /workspaces/<uuid>/items/<uuid>/catalog/namespaces/<schema>[/tables/<table>]. Metadata names use the current case-insensitive scoped resolution policy; catalog names retain exact spelling. Incomplete or visibility-limited misses remain unknown. JSON true/false results exit 0; null observations exit 1. No physical OneLake probes, mutations, atomic preconditions, persistent existence cache or implicit sign-in.")
        .arg(Arg::new("path").required(true).num_args(1))
        .arg(Arg::new("max-bytes").long("max-bytes").num_args(1)
            .value_parser(clap::value_parser!(NonZeroUsize)))
        .arg(Arg::new("timeout").long("timeout").num_args(1)
            .value_name("SECONDS").value_parser(clap::value_parser!(NonZeroUsize)))
}

fn selector(value: &str) -> Result<MetadataSelector, ExistsArgumentError> {
    if is_id(value) {
        MetadataSelector::id(value)
    } else {
        MetadataSelector::name(value)
    }
    .map_err(|_| ExistsArgumentError::Target)
}

/// Parse `exists` plus arguments (without the executable) without any host I/O.
pub fn parse_exists(
    args: impl IntoIterator<Item = OsString>,
) -> Result<ExistsCommand, ExistsArgumentError> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some(OsStr::new("exists")) {
        return Err(ExistsArgumentError::Target);
    }
    let matches = exists_command()
        .no_binary_name(true)
        .try_get_matches_from(args)
        .map_err(|error| ExistsArgumentError::Syntax(error.kind()))?;
    let path = matches
        .get_one::<String>("path")
        .ok_or(ExistsArgumentError::Target)?;
    if path.len() > fabric_sdk::discovery::MAX_TARGET_BYTES {
        return Err(ExistsArgumentError::Target);
    }
    let segments: Vec<_> = path.split('/').collect();
    let catalog = match segments.as_slice() {
        [
            "",
            "workspaces",
            workspace,
            "items",
            lakehouse,
            "catalog",
            "namespaces",
            namespace,
        ] if is_id(workspace) && is_id(lakehouse) => Some(ExistsTarget::CatalogNamespace {
            workspace: MetadataSelector::id(workspace).map_err(|_| ExistsArgumentError::Target)?,
            lakehouse: MetadataSelector::id(lakehouse).map_err(|_| ExistsArgumentError::Target)?,
            namespace: (*namespace).into(),
        }),
        [
            "",
            "workspaces",
            workspace,
            "items",
            lakehouse,
            "catalog",
            "namespaces",
            namespace,
            "tables",
            table,
        ] if is_id(workspace) && is_id(lakehouse) => Some(ExistsTarget::CatalogTable {
            workspace: MetadataSelector::id(workspace).map_err(|_| ExistsArgumentError::Target)?,
            lakehouse: MetadataSelector::id(lakehouse).map_err(|_| ExistsArgumentError::Target)?,
            namespace: (*namespace).into(),
            table: (*table).into(),
        }),
        _ => None,
    };
    let target = if let Some(target) = catalog {
        target
    } else {
        match GetTarget::parse(path).map_err(|_| ExistsArgumentError::Target)? {
            GetTarget::Workspace { workspace } => ExistsTarget::Workspace {
                workspace: selector(&workspace)?,
            },
            GetTarget::Item {
                workspace,
                item,
                item_type,
                representation: GetRepresentation::Metadata,
            } => ExistsTarget::Item {
                workspace: selector(&workspace)?,
                item: selector(&item)?,
                item_type,
            },
            _ => return Err(ExistsArgumentError::Target),
        }
    };
    // Validate type/selector invariants now, before an identity host is selected.
    ExistsRequest::new("00000000-0000-0000-0000-000000000000", target.clone())
        .map_err(|_| ExistsArgumentError::Target)?;
    let max_bytes = matches
        .get_one::<NonZeroUsize>("max-bytes")
        .copied()
        .unwrap_or(ExistsLimits::default().max_output_bytes);
    let timeout = matches
        .get_one::<NonZeroUsize>("timeout")
        .map(|seconds| Duration::from_secs(seconds.get() as u64))
        .unwrap_or(crate::get_command::DEFAULT_TIMEOUT);
    Ok(ExistsCommand {
        target,
        max_bytes,
        timeout,
    })
}

impl ExistsCommand {
    /// Bind the validated target to the host's explicit tenant without resolving it.
    pub fn request(&self, tenant: &str) -> Result<ExistsRequest, ExistsError> {
        ExistsRequest::new(tenant, self.target.clone())
    }

    /// Apply the selected byte cap to the existing finite paging/retention defaults.
    pub fn limits(&self) -> Result<ExistsLimits, RetentionError> {
        let mut limits = ExistsLimits::default();
        let defaults = limits.retention;
        let bytes = self.max_bytes.get();
        let response = defaults.max_response_bytes().min(bytes);
        limits.retention = RetentionLimits::new(
            defaults.max_responses(),
            bytes,
            response,
            defaults.max_header_bytes().min(response),
            defaults.max_headers(),
        )?;
        let response = NonZeroUsize::new(response).expect("positive input and default bounds");
        limits.workspaces.max_response_bytes = response;
        limits.items.max_response_bytes = response;
        limits.max_output_bytes = self.max_bytes;
        Ok(limits)
    }
}

/// Deliver the same canonical SDK report, retaining null/non-success evidence.
///
/// Determinate absence is a completed probe, not a service success or mutation
/// guard. Writer failures propagate; they never cause a second read or replay.
pub fn write_exists(report: &ExistsReport, stdout: &mut impl Write) -> io::Result<u8> {
    stdout.write_all(report.json_bytes())?;
    stdout.flush()?;
    Ok(u8::from(report.exists().is_none()))
}
