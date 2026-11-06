// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit data-only exports, separate from operation execution and receipts.

use crate::parser::Invocation;
use clap::{Arg, Command};
use fabric_cli_output::{
    OutputStream, RenderPlan,
    tabular::{TabularError, TabularFormat, TabularLimits, TabularTable, encode},
    text::TextPlan,
};
use serde_json::{Value, json};
use std::{
    fs,
    future::Future,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

mod files;
mod receipt;
mod schema;
pub use files::FileSink;
pub use receipt::ReceiptWriteFailure;

/// Explicit export options; both flags are required and no extension is inferred.
#[derive(Clone, Debug)]
pub struct ExportRequest {
    /// Codec selected independently of the destination extension.
    pub format: TabularFormat,
    /// Caller-selected destination, never stdout.
    pub output: PathBuf,
}

/// A parsed export around an otherwise unchanged production invocation.
#[derive(Clone, Debug)]
pub struct ExportCommand {
    /// Original production operation, executed at most once.
    pub command: Box<Invocation>,
    /// Independent file-delivery request.
    pub request: ExportRequest,
}

/// Declared complete native resultsets, not guessed arrays from rendered output.
#[derive(Clone, Debug)]
pub struct TableResult {
    /// Stable source label, such as `ls`, `query`, or `run`.
    pub source: String,
    /// All resultsets; single-file delivery rejects anything other than one.
    pub resultsets: Vec<Vec<Value>>,
    /// A successful operation whose native value is not a declared collection.
    pub validation_error: Option<String>,
    /// Explicit metadata columns, never inferred from arbitrary output shape.
    pub json_columns: Vec<String>,
    /// Operation metadata retained in the receipt, never inferred as row columns.
    pub metadata: Value,
    /// Validated zero-row table with genuine command/driver columns, never placeholders.
    pub empty_schema: Option<TabularTable>,
    /// Complete validated native table, preserving declared types and nullability.
    ///
    /// The one raw resultset must be empty and no inference overrides may be set;
    /// conflicting raw/typed representations fail rather than discard either.
    pub declared_table: Option<Box<TabularTable>>,
}

impl TableResult {
    /// Declare one complete resultset from a command-specific row adapter.
    pub fn single(source: impl Into<String>, rows: Vec<Value>) -> Self {
        Self {
            source: source.into(),
            resultsets: vec![rows],
            validation_error: None,
            json_columns: Vec::new(),
            metadata: Value::Null,
            empty_schema: None,
            declared_table: None,
        }
    }

    /// Supply native columns and values without JSON-based schema inference.
    pub fn from_table(source: impl Into<String>, table: TabularTable) -> Self {
        let mut result = Self::single(source, Vec::new());
        result.declared_table = Some(Box::new(table));
        result
    }

    /// Retain execution success without misrepresenting scalar metadata as rows.
    pub fn invalid(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            resultsets: Vec::new(),
            validation_error: Some("The native result is not a complete row collection".into()),
            json_columns: Vec::new(),
            metadata: Value::Null,
            empty_schema: None,
            declared_table: None,
        }
    }
}

/// An operation failure; partial rows are deliberately not accepted here.
#[derive(Clone, Debug)]
pub struct ExecutionFailure {
    /// Stable native command error code.
    pub code: String,
    /// Sanitized native diagnostic.
    pub message: String,
}

/// Failure to validate one complete native result as a single exportable table.
#[derive(Debug, thiserror::Error)]
pub enum ResultValidationError {
    /// The successful native value is not a declared complete row collection.
    #[error("The native result is not a complete row collection")]
    InvalidShape,
    /// Single-file delivery cannot represent this many resultsets.
    #[error("Single-file export requires one resultset, received {0}")]
    ResultSetCount(usize),
    /// Common table shape, type, JSON-column or allocation-bound validation failed.
    #[error(transparent)]
    Table(#[from] TabularError),
}

/// Validate successful native results without executing, encoding, or writing.
///
/// Operation metadata never participates in row/schema inference. Callers must
/// supply complete successful native results, not partial pages or diagnostics.
/// Empty dynamic/projected results need an actual validated `empty_schema` for
/// Parquet; JSON/CSV can still represent a genuinely zero-column empty result.
pub fn validate_result(result: &TableResult) -> Result<TabularTable, ResultValidationError> {
    if result.validation_error.is_some() {
        return Err(ResultValidationError::InvalidShape);
    }
    if result.resultsets.len() != 1 {
        return Err(ResultValidationError::ResultSetCount(
            result.resultsets.len(),
        ));
    }
    if let Some(table) = &result.declared_table {
        if !result.resultsets[0].is_empty()
            || !result.json_columns.is_empty()
            || result.empty_schema.is_some()
        {
            return Err(ResultValidationError::InvalidShape);
        }
        return Ok(table.as_ref().clone());
    }
    if result.resultsets[0].is_empty()
        && let Some(table) = &result.empty_schema
    {
        return if table.row_count() == 0 {
            Ok(table.clone())
        } else {
            Err(ResultValidationError::InvalidShape)
        };
    }
    let max_columns = TabularLimits::default().max_columns;
    if result.json_columns.len() > max_columns {
        return Err(TabularError::LimitExceeded {
            limit: "columns",
            max: max_columns,
        }
        .into());
    }
    let columns: Vec<_> = result.json_columns.iter().map(String::as_str).collect();
    TabularTable::from_rows_with_json_columns(&result.resultsets[0], &columns).map_err(Into::into)
}

/// Delivery outcome retained even if writing its JSON receipt subsequently fails.
#[derive(Debug)]
pub struct ExportDelivery {
    /// Execution and delivery evidence; never the actual table bytes.
    pub receipt: Value,
    /// Nonzero for either execution or delivery failure.
    pub exit_code: u8,
}

impl ExportDelivery {
    /// Retain a host/configuration failure as an explicit export receipt.
    pub fn host_failed(
        source: &str,
        request: &ExportRequest,
        code: &str,
        message: &str,
        exit_code: u8,
    ) -> Self {
        let mut delivery = with_destination(
            failed(source, "failed", "execution", code, message),
            request,
        );
        delivery.exit_code = exit_code;
        delivery
    }

    /// Report a rejected destination without claiming execution occurred.
    pub fn preflight_failed(source: &str, request: &ExportRequest) -> Self {
        with_destination(
            failed(
                source,
                "not_started",
                "preflight",
                "export_preflight_failed",
                "Export destination is unavailable or already exists",
            ),
            request,
        )
    }

    /// Encode the operation receipt without replaying execution or file delivery.
    pub fn plan(&self) -> io::Result<TextPlan> {
        let mut payload = serde_json::to_vec(&self.receipt).map_err(io::Error::other)?;
        payload.push(b'\n');
        Ok(TextPlan {
            records: vec![RenderPlan {
                stream: OutputStream::Stdout,
                exit_code: self.exit_code,
                payload,
            }],
            exit_code: self.exit_code,
        })
    }

    /// Write once; on failure this borrowed object retains execution/file evidence.
    ///
    /// Production hosts should use [`Self::write_to_streams`] to carry typed
    /// evidence through owned I/O failures and report bounded recovery metadata.
    pub fn write_to(&self, stdout: &mut impl Write) -> io::Result<u8> {
        let plan = self.plan()?;
        for record in plan.records {
            stdout.write_all(&record.payload)?;
        }
        Ok(self.exit_code)
    }
}

/// Single-file destination seam, allowing deterministic offline delivery failures.
pub trait ExportSink {
    /// Check no-clobber policy before executing any service operation.
    fn preflight(&mut self, request: &ExportRequest) -> io::Result<()>;
    /// Publish only complete encoded bytes, atomically and without replacement.
    fn publish(&mut self, request: &ExportRequest, bytes: &[u8]) -> io::Result<()>;
}

fn checked_destination(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty()
        || directory_spelling(path)
        || path == Path::new("-")
        || path.file_name().is_none()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid export destination",
        ));
    }
    let absolute = std::path::absolute(path)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid export destination"))?;
    for ancestor in parent.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Export directory must not contain links or reparse points",
            ));
        }
    }
    match fs::symlink_metadata(&absolute) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Export destination already exists",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(absolute),
        Err(error) => Err(error),
    }
}

fn directory_spelling(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    let separator = |byte: u8| byte == b'/' || (cfg!(windows) && byte == b'\\');
    bytes.last().is_some_and(|last| separator(*last))
        || (bytes.last() == Some(&b'.') && (bytes.len() == 1 || separator(bytes[bytes.len() - 2])))
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Check a destination before native credential/runtime construction.
///
/// The delivery entry point repeats this check immediately before execution.
pub fn preflight(request: &ExportRequest) -> io::Result<()> {
    checked_destination(&request.output).map(|_| ())
}

fn failed(source: &str, execution: &str, stage: &str, code: &str, message: &str) -> ExportDelivery {
    ExportDelivery {
        receipt: json!({
            "source": source,
            "execution": {"status": execution},
            "delivery": {"status": "failed", "stage": stage, "error": {"code": code, "message": message}}
        }),
        exit_code: if execution == "failed" && code == "Unauthorized" {
            4
        } else {
            1
        },
    }
}

async fn deliver_inner<F, Fut>(
    request: &ExportRequest,
    source: &str,
    known_resultsets: Option<usize>,
    sink: &mut impl ExportSink,
    execute: F,
) -> ExportDelivery
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<TableResult, ExecutionFailure>>,
{
    if known_resultsets.is_some_and(|count| count != 1) {
        return failed(
            source,
            "not_started",
            "preflight",
            "multiple_resultsets",
            "Single-file export requires exactly one resultset",
        );
    }

    if sink.preflight(request).is_err() {
        return failed(
            source,
            "not_started",
            "preflight",
            "export_preflight_failed",
            "Export destination is unavailable or already exists",
        );
    }
    let result = match execute().await {
        Ok(result) => result,
        Err(error) => return failed(source, "failed", "execution", &error.code, &error.message),
    };
    let retain_metadata = |mut delivery: ExportDelivery| {
        delivery.receipt["execution"]["metadata"] = result.metadata.clone();
        delivery
    };
    let table = match validate_result(&result) {
        Ok(table) => table,
        Err(ResultValidationError::ResultSetCount(_)) => {
            return retain_metadata(failed(
                &result.source,
                "succeeded",
                "validation",
                "multiple_resultsets",
                "Single-file export requires exactly one resultset",
            ));
        }
        Err(_) => {
            return retain_metadata(failed(
                &result.source,
                "succeeded",
                "validation",
                "invalid_table",
                "Native data cannot be represented as a complete table",
            ));
        }
    };
    let row_count = table.row_count();
    let schema = json!(table.columns());
    let evidence = |mut delivery: ExportDelivery| {
        delivery.receipt["delivery"]["rows"] = json!(row_count);
        delivery.receipt["delivery"]["schema"] = json!(schema);
        retain_metadata(delivery)
    };
    if request.format == TabularFormat::Parquet && table.columns().is_empty() {
        return evidence(failed(
            &result.source,
            "succeeded",
            "validation",
            "schema_unavailable",
            "This empty result has no declared schema; an arbitrary projection or dynamic native adapter must supply its real columns",
        ));
    }
    let bytes = match encode(&table, request.format) {
        Ok(bytes) => bytes,
        Err(_) => {
            return evidence(failed(
                &result.source,
                "succeeded",
                "encoding",
                "export_encoding_failed",
                "Encoding the table failed",
            ));
        }
    };
    if sink.publish(request, &bytes).is_err() {
        return evidence(failed(
            &result.source,
            "succeeded",
            "file",
            "export_delivery_failed",
            "Publishing the complete export failed",
        ));
    }
    ExportDelivery {
        receipt: json!({
            "source": result.source,
            "execution": {"status": "succeeded", "metadata": result.metadata},
            "delivery": {
                "status": "succeeded",
                "file": request.output.to_string_lossy(),
                "format": format_name(request.format),
                "bytes": bytes.len(),
                "rows": row_count,
                "schema": schema
            }
        }),
        exit_code: 0,
    }
}

/// Identify only the declared table-producing leaves, never arbitrary API data.
pub fn source(command: &Invocation) -> Option<&'static str> {
    use crate::parser::{JobReadKind, ReadKind};
    use fabric_cli_resources::frontdoor::AccessKind;
    match command {
        Invocation::NativeTabular(_) => None,
        Invocation::Read(command) if command.kind == ReadKind::List => Some("ls"),
        Invocation::JobRead(command) if command.kind == JobReadKind::List => Some("job run-list"),
        Invocation::Local(command)
            if command.command == fabric_cli_local::LocalCommand::ConfigList =>
        {
            Some("config ls")
        }
        Invocation::Access(command) => match command.kind {
            AccessKind::AclList { .. } => Some("acl ls"),
            AccessKind::AclGet { .. } => Some("acl get"),
            AccessKind::Find(_) => Some("find"),
            AccessKind::LabelListLocal => Some("label list-local"),
            _ => None,
        },
        Invocation::Table(command)
            if command.operation == fabric_cli_tables::TableOperation::Schema =>
        {
            Some("table schema")
        }
        _ => None,
    }
}

/// Command-specific native collection adapter; no rendered JSON is parsed.
///
/// Silent ACL-list/find results mean an empty collection in those handlers.
/// Missing labels mean missing configuration, not an empty successful dataset.
pub fn adapt(command: &Invocation, data: Option<Value>) -> TableResult {
    let Some(source) = source(command) else {
        return TableResult::invalid("unsupported");
    };
    match data {
        Some(Value::Array(rows)) => adapted_rows(command, source, rows),
        None if matches!(source, "ls" | "acl ls" | "find" | "job run-list") => {
            adapted_rows(command, source, Vec::new())
        }
        None if matches!(command, Invocation::Access(command)
                if matches!(&command.kind, fabric_cli_resources::frontdoor::AccessKind::AclGet { query: Some(query), .. } if query == ".")) =>
        {
            adapted_rows(command, source, Vec::new())
        }
        _ => TableResult::invalid(source),
    }
}

fn adapted_rows(command: &Invocation, source: &str, rows: Vec<Value>) -> TableResult {
    let mut result = declared_rows(source, rows);
    result.empty_schema = schema::command(command);
    result
}

fn declared_rows(source: &str, rows: Vec<Value>) -> TableResult {
    let columns: &[&str] = match source {
        "table schema" => &["type", "metadata"],
        "config ls" => &["value"],
        _ => &[],
    };
    let mut result = TableResult::single(source, rows);
    result.json_columns = columns
        .iter()
        .copied()
        .filter(|name| {
            result.resultsets[0]
                .iter()
                .any(|row| row.get(*name).is_some())
        })
        .map(str::to_owned)
        .collect();
    result
}

#[cfg(feature = "native-host")]
pub(crate) fn access_schema(
    command: &fabric_cli_resources::frontdoor::AccessCommand,
    target: Option<fabric_cli_context::NodeKind>,
) -> Option<TabularTable> {
    schema::access(command, target)
}

pub(crate) fn command_failure(error: &fabric_cli_output::CommandError) -> ExecutionFailure {
    let timestamp =
        fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z").expect("fixed timestamp");
    let envelope = error.envelope(timestamp, None);
    ExecutionFailure {
        code: envelope
            .result()
            .error_code()
            .unwrap_or("command_failed")
            .into(),
        message: envelope
            .result()
            .message()
            .unwrap_or("Operation failed")
            .into(),
    }
}

/// Export configuration through its declared ordered setting/value adapter.
///
/// The legacy local output has private row storage, so this adapter uses the
/// same exported setting registry and initialized configuration snapshot.
pub fn config_rows(store: &fabric_cli_local::ConfigStore) -> Result<TableResult, ExecutionFailure> {
    let settings = store
        .initialize()
        .map_err(|error| command_failure(&error.command_error()))?;
    Ok(declared_rows("config ls", fabric_cli_local::SETTINGS.iter().map(|setting| json!({
            "setting": setting.key,
            "value": settings.get(setting.key).cloned().unwrap_or_else(|| Value::String(String::new()))
        })).collect()))
}

/// Adapt local labels without identity, remote metadata or another runtime.
pub fn label_rows(
    command: &Invocation,
    definitions: Option<&Path>,
) -> Result<TableResult, ExecutionFailure> {
    let Invocation::Access(access) = command else {
        return Ok(TableResult::invalid("label list-local"));
    };
    match fabric_cli_resources::frontdoor::execute_local(access, definitions) {
        Some(Ok(value)) => Ok(adapt(command, value.data)),
        Some(Err(error)) => Err(ExecutionFailure {
            code: error.code,
            message: error.message,
        }),
        None => Ok(TableResult::invalid("label list-local")),
    }
}

/// Dispatch the two local-only exporters without constructing an async runtime.
///
/// Returns `None` for commands requiring the caller's existing remote host.
pub fn dispatch_local(
    export: &ExportCommand,
    store: &fabric_cli_local::ConfigStore,
    sink: &mut impl ExportSink,
) -> Option<ExportDelivery> {
    let source = source(&export.command)?;
    if !matches!(source, "config ls" | "label list-local") {
        return None;
    }
    use futures_util::FutureExt;
    Some(
        dispatch(export, sink, |_| {
            std::future::ready(if source == "config ls" {
                config_rows(store)
            } else {
                store
                    .initialize()
                    .map_err(|error| command_failure(&error.command_error()))
                    .and_then(|settings| {
                        let definitions = settings
                            .get("local_definition_labels")
                            .and_then(Value::as_str)
                            .map(Path::new);
                        label_rows(&export.command, definitions)
                    })
            })
        })
        .now_or_never()
        .expect("local export contains no asynchronous operations"),
    )
}

/// Execute LS using the existing native metadata/session and pre-render value.
pub async fn read_rows(
    session: &crate::session::ReadSession<'_>,
    command: &crate::parser::ReadCommand,
) -> Result<TableResult, ExecutionFailure> {
    if command.kind != crate::parser::ReadKind::List {
        return Err(ExecutionFailure {
            code: "not_tabular".into(),
            message: "Only LS declares a tabular read result".into(),
        });
    }
    session
        .execute_with_target(command, None, std::future::pending())
        .await
        .map(|(input, target)| {
            let mut result = input.map_or_else(
                || TableResult::invalid("ls"),
                |input| adapt(&Invocation::Read(command.clone()), input.data),
            );
            result.empty_schema = schema::listing(command, target.kind());
            result
        })
        .map_err(|failure| command_failure(&failure.command))
}

/// Execute run-list using the existing job host; status/progress are not rows.
pub async fn job_rows(
    session: &crate::jobs::JobReadSession<'_, '_>,
    command: &crate::parser::JobReadCommand,
) -> Result<TableResult, ExecutionFailure> {
    if command.kind != crate::parser::JobReadKind::List {
        return Err(ExecutionFailure {
            code: "not_tabular".into(),
            message: "Only run-list declares a tabular job result".into(),
        });
    }
    let host = crate::jobs::JobSession {
        reads: session.reads,
        jobs: std::sync::Arc::clone(&session.jobs),
        files: &fabric_cli_jobs::NoFiles,
        ui: &crate::jobs::ReadOnlyUi,
        config: fabric_cli_jobs::JobConfig::default(),
    };
    host.execute_tabular_list(&command.family_command())
        .await
        .map(|output| {
            output.map_or_else(
                || TableResult::invalid("job run-list"),
                |output| adapt(&Invocation::JobRead(command.clone()), output.input.data),
            )
        })
        .map_err(|error| {
            let envelope = error.envelope(session.reads.timestamp.clone(), None);
            ExecutionFailure {
                code: envelope
                    .result()
                    .error_code()
                    .unwrap_or("job_failed")
                    .into(),
                message: envelope
                    .result()
                    .message()
                    .unwrap_or("Job listing failed")
                    .into(),
            }
        })
}

/// Adapt retained access/table evidence, never operation messages or stdout text.
#[cfg(feature = "native-host")]
pub fn family_rows(
    command: &Invocation,
    output: crate::families::FamilyOutput,
) -> Result<TableResult, ExecutionFailure> {
    use crate::families::FamilyEvidence;
    match output.evidence {
        FamilyEvidence::Access(Ok(value)) => Ok(adapt(command, value.data)),
        FamilyEvidence::Access(Err(error)) => Err(ExecutionFailure {
            code: error.code,
            message: error.message,
        }),
        FamilyEvidence::AccessTable {
            result,
            empty_schema,
        } => match result {
            Ok(value) => {
                let mut result = adapt(command, value.data);
                result.empty_schema = empty_schema.or(result.empty_schema);
                Ok(result)
            }
            Err(error) => Err(ExecutionFailure {
                code: error.code,
                message: error.message,
            }),
        },
        FamilyEvidence::Table(evidence) => match evidence.result {
            Ok(output) => Ok(adapt(command, output.input.and_then(|input| input.data))),
            Err(error) => {
                let timestamp = fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z")
                    .expect("fixed timestamp");
                let envelope = error.envelope(timestamp, None);
                Err(ExecutionFailure {
                    code: envelope
                        .result()
                        .error_code()
                        .unwrap_or("table_failed")
                        .into(),
                    message: envelope
                        .result()
                        .message()
                        .unwrap_or("Schema retrieval failed")
                        .into(),
                })
            }
        },
        FamilyEvidence::CommandFailure(error) => Err(command_failure(&error)),
        _ => Ok(TableResult::invalid(
            source(command).unwrap_or("unsupported"),
        )),
    }
}
/// Dispatch a parsed explicit export through an injected native operation.
///
/// The real executable and offline command-boundary tests share this entry.
/// Only declared producers are accepted; no universal array-shape guessing or
/// stdout reparsing is performed here.
pub async fn dispatch<'a, F, Fut>(
    export: &'a ExportCommand,
    sink: &mut impl ExportSink,
    execute: F,
) -> ExportDelivery
where
    F: FnOnce(&'a Invocation) -> Fut,
    Fut: Future<Output = Result<TableResult, ExecutionFailure>>,
{
    let Some(source) = source(&export.command) else {
        return failed(
            "unsupported",
            "not_started",
            "preflight",
            "not_tabular",
            "This command does not declare a tabular result",
        );
    };
    deliver(&export.request, source, Some(1), sink, || {
        execute(&export.command)
    })
    .await
}

fn format_name(format: TabularFormat) -> &'static str {
    match format {
        TabularFormat::Csv => "csv",
        TabularFormat::Json => "json",
        TabularFormat::Parquet => "parquet",
    }
}

fn with_destination(mut delivery: ExportDelivery, request: &ExportRequest) -> ExportDelivery {
    delivery.receipt["delivery"]["file"] = json!(request.output.to_string_lossy());
    delivery.receipt["delivery"]["format"] = json!(format_name(request.format));
    delivery
}

/// Deliver a complete native table with separate execution and file evidence.
///
/// Known multiple resultsets are rejected before `execute`; unknown counts are
/// validated after execution. Neither a codec, sink nor receipt failure replays
/// the operation. Common codec limits bound all materialized export buffers.
pub async fn deliver<F, Fut>(
    request: &ExportRequest,
    source: &str,
    known_resultsets: Option<usize>,
    sink: &mut impl ExportSink,
    execute: F,
) -> ExportDelivery
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<TableResult, ExecutionFailure>>,
{
    with_destination(
        deliver_inner(request, source, known_resultsets, sink, execute).await,
        request,
    )
}

pub(crate) fn register(mut command: Command) -> Command {
    for path in [
        &["ls"][..],
        &["job", "run-list"][..],
        &["acl", "ls"][..],
        &["acl", "get"][..],
        &["find"][..],
        &["label", "list-local"][..],
        &["config", "ls"][..],
        &["table", "schema"][..],
    ] {
        command = mount(command, path);
    }
    command
}

fn mount(command: Command, path: &[&str]) -> Command {
    let mut mounted = false;
    // Mutating one child removes and appends it; preserve declaration/help order.
    let command = command.mut_subcommands(|leaf| {
        if leaf.get_name() != path[0] {
            return leaf;
        }
        mounted = true;
        if path.len() > 1 {
            return mount(leaf, &path[1..]);
        }
        let existing_output = leaf.get_arguments().any(|arg| arg.get_id() == "output");
        build_arguments(leaf, Some("output_format"), existing_output)
            .expect("declared tabular leaves have no conflicting input-format flag")
    });
    assert!(mounted, "declared tabular command is registered");
    command
}

#[cfg(test)]
#[test]
fn mounting_preserves_command_and_leaf_declaration_order() {
    let command = Command::new("root")
        .arg(Arg::new("output_format").long("output_format").global(true))
        .subcommand(Command::new("before"))
        .subcommand(
            Command::new("job")
                .subcommand(Command::new("start"))
                .subcommand(Command::new("run-list"))
                .subcommand(Command::new("run")),
        )
        .subcommand(Command::new("after"));
    let command = mount(command, &["job", "run-list"]);
    assert_eq!(
        command
            .get_subcommands()
            .map(Command::get_name)
            .collect::<Vec<_>>(),
        ["before", "job", "after"]
    );
    let job = command.find_subcommand("job").unwrap();
    assert_eq!(
        job.get_subcommands()
            .map(Command::get_name)
            .collect::<Vec<_>>(),
        ["start", "run-list", "run"]
    );
    assert!(
        job.find_subcommand("run-list")
            .unwrap()
            .get_arguments()
            .any(|argument| argument.get_id() == "tabular_format")
    );
}

/// A leaf's existing input/output option prevents mounting shared export flags.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("Shared tabular export conflicts with an existing --{flag} option")]
pub struct ExportArgumentsError {
    /// Conflicting registered option name, never a supplied argument value.
    pub flag: &'static str,
}

/// Add identical explicit export flags to a real table-producing command leaf.
///
/// `presentation_flag` is the registered legacy presentation argument ID (such
/// as inherited `output_format`), or `None` when the host has no such argument.
/// Existing `--format`/`--output` inputs are rejected rather than overwritten.
/// Mounting this builder does not register a backend or execute an operation.
pub fn arguments(
    command: Command,
    presentation_flag: Option<&'static str>,
) -> Result<Command, ExportArgumentsError> {
    build_arguments(command, presentation_flag, false)
}

fn build_arguments(
    command: Command,
    presentation_flag: Option<&'static str>,
    reuse_output: bool,
) -> Result<Command, ExportArgumentsError> {
    if command
        .get_arguments()
        .any(|arg| arg.get_long() == Some("format") || arg.get_id() == "tabular_format")
    {
        return Err(ExportArgumentsError { flag: "format" });
    }
    if command.get_arguments().any(|arg| {
        arg.get_id() == "tabular_output"
            || (!reuse_output && (arg.get_long() == Some("output") || arg.get_id() == "output"))
    }) {
        return Err(ExportArgumentsError { flag: "output" });
    }
    let mut format = Arg::new("tabular_format")
        .long("format")
        .value_parser(["csv", "json", "parquet"])
        .requires(if reuse_output {
            "output"
        } else {
            "tabular_output"
        });
    if let Some(flag) = presentation_flag {
        format = format.conflicts_with(flag);
    }
    let command = command.arg(format);
    Ok(if reuse_output {
        command
    } else {
        command.arg(
            Arg::new("tabular_output")
                .long("output")
                .value_name("FILE")
                .requires("tabular_format"),
        )
    })
}

/// Extract validated export flags from a leaf or its enclosing root matches.
///
/// This performs no filesystem or backend access and never infers an extension.
pub fn request(matches: &clap::ArgMatches) -> Option<ExportRequest> {
    let mut leaf = matches;
    while let Some((_, child)) = leaf.subcommand() {
        leaf = child;
    }
    let format = leaf
        .try_get_one::<String>("tabular_format")
        .ok()
        .flatten()?;
    let output = leaf
        .try_get_one::<String>("tabular_output")
        .ok()
        .flatten()
        .or_else(|| leaf.try_get_one::<String>("output").ok().flatten())?;
    Some(ExportRequest {
        format: match format.as_str() {
            "csv" => TabularFormat::Csv,
            "json" => TabularFormat::Json,
            "parquet" => TabularFormat::Parquet,
            _ => return None,
        },
        output: output.into(),
    })
}
