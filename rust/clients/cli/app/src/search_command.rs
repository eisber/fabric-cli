// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit search arguments and bounded local delivery, without identity or I/O discovery.

use clap::{Arg, ArgAction, Command, error::ErrorKind};
use fabric_cli_storage_files::{ExactOutputError, ExactOutputFile};
use fabric_sdk::search::{
    CatalogCursor, CatalogResume, InitialSearch, SearchError, SearchLimits, SearchMode,
    SearchRequest, SearchScope, TypeFilters,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt,
    io::{self, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::Duration,
};

/// One parsed search invocation; provider validation remains SDK-owned.
#[derive(Clone)]
pub struct SearchCommand {
    /// Explicit `/` tenant collection or `/workspaces/<uuid>` collection.
    pub path: String,
    /// Inspect the common SDK operation schema without executing providers.
    pub schema: bool,
    /// Explicit metadata, hosted relevance or federated behavior.
    pub mode: SearchMode,
    /// Original query text; never expanded or rewritten.
    pub text: Option<String>,
    /// Included native types, not a local coverage guarantee.
    pub types: Vec<String>,
    /// Excluded native types.
    pub exclude_types: Vec<String>,
    /// Native Catalog cursor retaining its original explicit scope and opaque token.
    pub cursor: Option<CatalogCursor>,
    /// Finite page/result cap.
    pub limit: usize,
    /// Bound for each acquired response and serialized delivery.
    pub max_bytes: NonZeroUsize,
    /// One total host deadline, including credential acquisition.
    pub timeout: Duration,
    /// Local projection of data only, never source evidence.
    pub project: Option<String>,
    /// Exact no-clobber local output, or stdout.
    pub output: Option<PathBuf>,
}

impl fmt::Debug for SearchCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchCommand")
            .field("mode", &self.mode)
            .field("schema", &self.schema)
            .field("has_text", &self.text.is_some())
            .field("has_continuation", &self.cursor.is_some())
            .field("has_output_file", &self.output.is_some())
            .field("limit", &self.limit)
            .field("max_bytes", &self.max_bytes)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Sanitized argument failures never copy user text or paths into diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("Invalid search arguments; run fab-cli-v2 search --help.")]
pub enum SearchArgumentError {
    /// Missing command or rejected syntax.
    Syntax(ErrorKind),
    /// Unsupported target, mode, criteria combination or bound.
    InvalidInput,
}

/// Builds the thin CLI projection of the SDK search operation.
pub fn search_command() -> Command {
    let command = Command::new("search")
        .about("Preview bounded Catalog and explicit Relevance Engine discovery")
        .args_override_self(false)
        .infer_long_args(false)
        .after_help("Scope is / or /workspaces/<uuid>. Catalog is the default; relevance and hybrid explicitly opt into hosted ranked discovery. Catalog supports one native page and continuation; relevance is bounded top-k and hybrid has no combined cursor. Results are not a complete tenant inventory. Only JSON is supported. Provider capabilities and authorization remain authoritative.")
        .arg(Arg::new("path").required(true).num_args(1))
        .arg(Arg::new("schema").long("schema").action(ArgAction::SetTrue))
        .arg(Arg::new("mode").long("mode").num_args(1).default_value("catalog").value_parser(["catalog", "relevance", "hybrid"]))
        .arg(Arg::new("text").long("text").num_args(1))
        .arg(Arg::new("type").long("type").num_args(1).action(ArgAction::Append))
        .arg(Arg::new("exclude-type").long("exclude-type").num_args(1).action(ArgAction::Append))
        .arg(Arg::new("cursor").long("cursor").num_args(1).value_name("NATIVE_CURSOR_JSON"))
        .arg(Arg::new("limit").long("limit").num_args(1).default_value("50").value_parser(clap::value_parser!(usize)))
        .arg(Arg::new("max-bytes").long("max-bytes").num_args(1).default_value("1048576").value_parser(clap::value_parser!(NonZeroUsize)))
        .arg(Arg::new("timeout").long("timeout").num_args(1).default_value("30s").value_parser(parse_duration))
        .arg(Arg::new("format").long("format").num_args(1).default_value("json").value_parser(["json"]))
        .arg(Arg::new("project").long("project").short('q').num_args(1))
        .arg(Arg::new("output").long("output").short('o').num_args(1).value_parser(clap::builder::OsStringValueParser::new()));
    #[cfg(feature = "docs-search")]
    let command = command.before_help("For credential-free embedded documentation, use search docs --text QUERY or search docs --help. The scope and provider options below apply only to remote metadata discovery.");
    command
}

/// Parses search without reading environment, files, identity or network.
///
/// # Errors
/// Rejects invalid targets, repeated scalar options, incompatible resume fields
/// and bounds before provider setup. Help is a typed Clap display result.
pub fn parse_search(
    args: impl IntoIterator<Item = impl Into<OsString> + Clone>,
) -> Result<SearchCommand, SearchArgumentError> {
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if args.first().and_then(|value| value.to_str()) != Some("search") {
        return Err(SearchArgumentError::InvalidInput);
    }
    let matches = search_command()
        .try_get_matches_from(args)
        .map_err(|error| SearchArgumentError::Syntax(error.kind()))?;
    let mode = match matches.get_one::<String>("mode").map(String::as_str) {
        Some("catalog") => SearchMode::Catalog,
        Some("relevance") => SearchMode::Relevance,
        Some("hybrid") => SearchMode::Hybrid,
        _ => return Err(SearchArgumentError::InvalidInput),
    };
    let command = SearchCommand {
        path: matches
            .get_one::<String>("path")
            .cloned()
            .ok_or(SearchArgumentError::InvalidInput)?,
        schema: matches.get_flag("schema"),
        mode,
        text: matches.get_one::<String>("text").cloned(),
        types: matches
            .get_many::<String>("type")
            .map(|values| values.cloned().collect())
            .unwrap_or_default(),
        exclude_types: matches
            .get_many::<String>("exclude-type")
            .map(|values| values.cloned().collect())
            .unwrap_or_default(),
        cursor: matches
            .get_one::<String>("cursor")
            .map(|value| parse_cursor(value))
            .transpose()?,
        limit: *matches
            .get_one::<usize>("limit")
            .ok_or(SearchArgumentError::InvalidInput)?,
        max_bytes: *matches
            .get_one::<NonZeroUsize>("max-bytes")
            .ok_or(SearchArgumentError::InvalidInput)?,
        timeout: *matches
            .get_one::<Duration>("timeout")
            .ok_or(SearchArgumentError::InvalidInput)?,
        project: matches.get_one::<String>("project").cloned(),
        output: matches
            .get_one::<OsString>("output")
            .filter(|path| path.as_os_str() != "-")
            .map(PathBuf::from),
    };
    validate_scope(&command.path)?;
    if command.mode != SearchMode::Catalog
        && command
            .text
            .as_ref()
            .is_some_and(|text| text.encode_utf16().count() > 500)
    {
        return Err(SearchArgumentError::InvalidInput);
    }
    if command.limit == 0
        || command.mode == SearchMode::Hybrid && command.max_bytes.get() < 2
        || command.limit
            > if command.mode == SearchMode::Catalog {
                1000
            } else {
                50
            }
        || command
            .text
            .as_ref()
            .is_some_and(|text| text.trim().is_empty())
        || command
            .project
            .as_ref()
            .is_some_and(|query| query.trim().is_empty())
    {
        return Err(SearchArgumentError::InvalidInput);
    }
    if let Some(cursor) = &command.cursor {
        if command.mode != SearchMode::Catalog
            || command.text.is_some()
            || !command.types.is_empty()
            || !command.exclude_types.is_empty()
            || !cursor_matches_path(cursor, &command.path)?
        {
            return Err(SearchArgumentError::InvalidInput);
        }
    } else if !command.schema
        && command.text.is_none()
        && (command.mode != SearchMode::Catalog
            || (command.types.is_empty()
                && command.exclude_types.is_empty()
                && command.path == "/"))
    {
        return Err(SearchArgumentError::InvalidInput);
    }
    if command.schema
        && (command.text.is_some()
            || command.cursor.is_some()
            || !command.types.is_empty()
            || !command.exclude_types.is_empty()
            || command.project.is_some()
            || command.output.is_some())
    {
        return Err(SearchArgumentError::InvalidInput);
    }
    if let Some(query) = &command.project {
        fabric_cli_query::validate_projection(query)
            .map_err(|_| SearchArgumentError::InvalidInput)?;
    }
    Ok(command)
}

fn parse_cursor(value: &str) -> Result<CatalogCursor, SearchArgumentError> {
    let value: Value =
        serde_json::from_str(value).map_err(|_| SearchArgumentError::InvalidInput)?;
    match SearchRequest::from_value(json!({"kind":"catalogResume","cursor":value}))
        .map_err(|_| SearchArgumentError::InvalidInput)?
    {
        SearchRequest::CatalogResume(resume) => Ok(resume.cursor().clone()),
        SearchRequest::Initial(_) => Err(SearchArgumentError::InvalidInput),
    }
}

fn cursor_matches_path(cursor: &CatalogCursor, path: &str) -> Result<bool, SearchArgumentError> {
    Ok(
        match (cursor.scope().workspace_id(), validate_scope(path)?) {
            (None, None) => true,
            (Some(original), Some(selected)) => original.as_str().eq_ignore_ascii_case(selected),
            _ => false,
        },
    )
}

/// Binds CLI arguments to the same validated request accepted by SDK consumers.
///
/// The tenant comes from explicit host selection, not path guessing or discovery.
///
/// # Errors
/// Rejects invalid identities, typed filters and incompatible operation bounds.
pub fn sdk_request(command: &SearchCommand, tenant_id: &str) -> Result<SearchRequest, SearchError> {
    if command.schema
        || command.cursor.is_some()
            && (command.mode != SearchMode::Catalog
                || command.text.is_some()
                || !command.types.is_empty()
                || !command.exclude_types.is_empty())
    {
        return Err(SearchError::InvalidInput { field: "input" });
    }
    let workspace =
        validate_scope(&command.path).map_err(|_| SearchError::InvalidInput { field: "scope" })?;
    let scope = match workspace {
        Some(workspace) => SearchScope::workspace(tenant_id, workspace)?,
        None => SearchScope::tenant(tenant_id)?,
    };
    let limits = SearchLimits::new(
        command.limit,
        command.limit,
        command.max_bytes.get(),
        if command.mode == SearchMode::Hybrid {
            2
        } else {
            1
        },
    )?;
    if let Some(cursor) = &command.cursor {
        if cursor.scope() != &scope {
            return Err(SearchError::InvalidInput {
                field: "cursor.scope",
            });
        }
        Ok(SearchRequest::CatalogResume(CatalogResume::new(
            cursor.clone(),
            limits,
        )))
    } else {
        Ok(SearchRequest::Initial(InitialSearch::new(
            command.mode,
            scope,
            command.text.clone(),
            TypeFilters::new(command.types.clone(), command.exclude_types.clone())?,
            limits,
        )?))
    }
}

/// Returns the exact workspace selector, or no workspace for tenant scope.
///
/// # Errors
/// Rejects unsupported representations, names, URL syntax and malformed IDs.
pub fn validate_scope(path: &str) -> Result<Option<&str>, SearchArgumentError> {
    if path == "/" {
        return Ok(None);
    }
    let workspace = path
        .strip_prefix("/workspaces/")
        .ok_or(SearchArgumentError::InvalidInput)?;
    if workspace.len() != 36
        || !workspace.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(SearchArgumentError::InvalidInput);
    }
    Ok(Some(workspace))
}

fn parse_duration(value: &str) -> Result<Duration, &'static str> {
    let (number, unit) = ["ms", "s", "m", "h"]
        .into_iter()
        .find_map(|unit| value.strip_suffix(unit).map(|number| (number, unit)))
        .ok_or("Expected a positive integer duration with ms, s, m or h.")?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Expected a positive integer duration.");
    }
    let number: u64 = number.parse().map_err(|_| "Duration is out of range.")?;
    let multiplier = match unit {
        "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        _ => 3_600_000,
    };
    let milliseconds = number
        .checked_mul(multiplier)
        .ok_or("Duration is out of range.")?;
    if milliseconds == 0 || milliseconds > 86_400_000 {
        return Err("Duration must be positive and at most 24 hours.");
    }
    Ok(Duration::from_millis(milliseconds))
}

/// Preflighted local delivery; remote acquisition remains a separate operation.
#[derive(Debug)]
pub struct SearchDelivery {
    file: Option<ExactOutputFile>,
    project: Option<String>,
    max_bytes: usize,
}

/// Local delivery failures preserve their typed underlying error.
#[derive(Debug, thiserror::Error)]
pub enum SearchDeliveryError {
    /// Invalid or unavailable exact local destination.
    #[error("Search output preflight or commit failed.")]
    File(#[from] ExactOutputError),
    /// Serialized output exceeded the selected bound.
    #[error("Search output exceeded the selected byte bound.")]
    ByteLimit,
    /// The caller did not provide the search result envelope.
    #[error("Invalid search result envelope.")]
    Envelope,
    /// Projection failed without replacing source evidence.
    #[error("Search data projection failed.")]
    Projection(#[from] fabric_cli_query::QueryError),
    /// Output could not be serialized or written.
    #[error("Search output serialization or write failed.")]
    Output,
}

impl SearchDelivery {
    /// Returns the exact preflighted filename for immutable approval evidence.
    pub fn output_path(&self) -> Option<&Path> {
        self.file.as_ref().map(ExactOutputFile::path)
    }

    /// Validates output before remote work, without creating or replacing files.
    ///
    /// # Errors
    /// Rejects an existing destination or invalid absolute working directory.
    pub fn preflight(command: &SearchCommand, cwd: &Path) -> Result<Self, SearchDeliveryError> {
        if !cwd.is_absolute() {
            return Err(SearchDeliveryError::Envelope);
        }
        let file = command
            .output
            .as_ref()
            .map(|path| {
                ExactOutputFile::preflight(
                    &if path.is_absolute() {
                        path.clone()
                    } else {
                        cwd.join(path)
                    },
                    command.max_bytes.get(),
                )
            })
            .transpose()?;
        Ok(Self {
            file,
            project: command.project.clone(),
            max_bytes: command.max_bytes.get(),
        })
    }

    /// Projects data only, bounds complete JSON and writes to the selected sink.
    ///
    /// This is blocking host work, after acquisition and deadline checks.
    ///
    /// # Errors
    /// Fails explicitly on projection, bounds, exclusive-create or writer failure.
    pub fn deliver(
        self,
        mut envelope: Value,
        stdout: &mut dyn Write,
    ) -> Result<(), SearchDeliveryError> {
        let valid_outcome = match envelope.get("status").and_then(Value::as_str) {
            Some("succeeded" | "partial") => envelope.get("data").is_some(),
            Some("failed" | "unknown") => envelope.get("error").is_some_and(Value::is_object),
            Some("accepted") => true,
            _ => false,
        };
        if !envelope.is_object()
            || !envelope.get("meta").is_some_and(Value::is_object)
            || !valid_outcome
        {
            return Err(SearchDeliveryError::Envelope);
        }
        let original = serde_json::to_vec(&envelope).map_err(|_| SearchDeliveryError::Output)?;
        if original.len() > self.max_bytes {
            return Err(SearchDeliveryError::ByteLimit);
        }
        if let Some(query) = self.project.filter(|_| envelope.get("data").is_some()) {
            envelope["data"] = fabric_cli_query::project_value(&envelope["data"], &query)?;
        }
        let mut bytes = serde_json::to_vec(&envelope).map_err(|_| SearchDeliveryError::Output)?;
        bytes.push(b'\n');
        if bytes.len() > self.max_bytes {
            return Err(SearchDeliveryError::ByteLimit);
        }
        if let Some(file) = self.file {
            file.commit(&bytes)?;
        } else {
            stdout
                .write_all(&bytes)
                .map_err(|_| SearchDeliveryError::Output)?;
            stdout.flush().map_err(|_| SearchDeliveryError::Output)?;
        }
        Ok(())
    }
}

impl From<io::Error> for SearchDeliveryError {
    fn from(_: io::Error) -> Self {
        Self::Output
    }
}
