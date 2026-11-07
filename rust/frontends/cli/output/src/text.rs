// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure translation of `fab_ui` text branches, with caller-owned terminal policy.
//!
//! Width means Python East Asian F/W width, not grapheme/wcwidth: combining
//! marks still count one. Unicode properties are injected via [`CharacterWidth`],
//! avoiding an ambient Unicode database or silently approximate width tables.

use crate::{
    CommandError, Indent, OutputEnvelope, OutputError, OutputStatus, OutputStream, RenderPlan,
    encoding,
};
use serde_json::{Map, Value};
use std::io::{self, Write};
use thiserror::Error;

/// Finite budgets enforced by the production Unicode formatter and data loader.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum UnicodeResourceKind {
    /// Bytes in the embedded or caller-supplied property database.
    #[error("data bytes")]
    DataBytes,
    /// Aggregate UTF-8 bytes consumed by a formatting operation.
    #[error("input bytes")]
    InputBytes,
    /// UTF-8 bytes produced by a formatting operation.
    #[error("output bytes")]
    OutputBytes,
    /// Nested JSON container depth.
    #[error("depth")]
    Depth,
    /// JSON values and object keys visited by a formatting operation.
    #[error("nodes")]
    Nodes,
}

/// Explicit invalid-data classes for the production Unicode provider.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum UnicodeDataKind {
    /// The property database is not valid JSON of the required schema.
    #[error("malformed property database JSON")]
    MalformedJson,
    /// The source or Unicode versions and recorded provenance do not match.
    #[error("version or provenance mismatch")]
    Version,
    /// The caller attempted to raise an operation budget above the hard caps.
    #[error("operation limits exceed hard caps")]
    Limits,
    /// A property or mapping table exceeds the supported number of entries.
    #[error("table size")]
    TableSize,
    /// A range overlaps, contains surrogates, or has invalid property values.
    #[error("scalar coverage or property range")]
    ScalarCoverage,
    /// The database does not cover the complete valid-scalar domain.
    #[error("incomplete scalar domain")]
    IncompleteDomain,
    /// Full casing mappings are invalid, oversized, or not strictly ordered.
    #[error("case mapping")]
    CaseMapping,
    /// The Rich variation-selector widening set is invalid.
    #[error("Rich VS16 data")]
    RichVs16,
    /// An integer input denotes a surrogate or exceeds U+10FFFF.
    #[error("U+{0:04X} is not a Unicode scalar")]
    InvalidScalar(u32),
}

/// Source-visible render failures; callers retain control of dispatch and exit.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TextError {
    /// Source had already produced text before a subsequent render failure.
    #[error("{error}")]
    AfterOutput {
        /// Underlying failure.
        error: Box<TextError>,
        /// Ordered source output before that failure, available for caller delivery.
        partial: Box<TextPlan>,
    },
    /// Text rejects a result whose data, hidden data and message are all absent.
    #[error("Invalid result format provided - can't be None or empty")]
    InvalidResult,
    /// Key/value helper rejected a non-object/non-list top-level value.
    #[error("Invalid entries format")]
    InvalidEntries,
    /// The table helper uses the Labels error rather than the Common error.
    #[error(
        "The entries format in the labels definition file is invalid. Please check the file structure and ensure it follows the expected format"
    )]
    InvalidTableEntries,
    /// Source title-case validation failed.
    #[error("Invalid key format: '{0}'. Only underscore-separated words are allowed.")]
    InvalidKey(String),
    /// A heterogeneous table/key-value row failed at the source attribute access.
    #[error("'{kind}' object has no attribute '{method}'")]
    InvalidRow {
        /// Python runtime type name.
        kind: &'static str,
        /// Attribute used by the source branch.
        method: &'static str,
    },
    /// Explicitly unsupported terminal property, not a silent narrow-width default.
    #[error("No Python East Asian width supplied for U+{0:04X}")]
    MissingWidth(u32),
    /// Python Unicode casing is not approximated with Rust Unicode casing.
    #[error("Python title-case properties are required for non-ASCII key '{0}'")]
    UnicodeKey(String),
    /// Unicode repr printability was not supplied by a source-equivalent caller.
    #[error("Python repr properties are required for non-ASCII nested string '{0}'")]
    UnicodeRepr(String),
    /// A production Unicode operation exceeded a finite budget.
    #[error("Unicode {0} limit exceeded")]
    UnicodeResource(#[source] UnicodeResourceKind),
    /// The immutable production Unicode database could not be initialized.
    #[error("Invalid Unicode provider data: {0}")]
    UnicodeData(#[source] UnicodeDataKind),
    /// The format came from caller arguments/config but was not supported.
    #[error("Output format {0} not supported")]
    UnsupportedFormat(String),
    /// Nonempty help categories contained no command at all.
    #[error("max() iterable argument is empty")]
    EmptyHelp,
    /// JSON serialization failed.
    #[error(transparent)]
    Json(#[from] OutputError),
}

impl TextError {
    /// Map source errors to the ordinary JSON/text command-error contract.
    pub fn command_error(&self) -> CommandError {
        if let Self::AfterOutput { error, .. } = self {
            return error.command_error();
        }
        let code = match self {
            Self::InvalidResult => "InvalidInput",
            Self::InvalidEntries | Self::InvalidTableEntries => "InvalidEntriesFormat",
            Self::UnsupportedFormat(_) => "NotSupported",
            Self::Json(_) => "InvalidJson",
            Self::UnicodeResource(_) => "UnicodeResourceLimit",
            Self::UnicodeData(_) => "UnicodeData",
            _ => "UnexpectedError",
        };
        CommandError::new(Some(self.to_string()), Some(code.to_owned()))
    }

    /// Source output preceding a failure; rendering itself still performs no I/O.
    pub fn partial_output(&self) -> Option<&TextPlan> {
        match self {
            Self::AfterOutput { partial, .. } => Some(partial),
            _ => None,
        }
    }
}

/// Pure terminal character-property provider.
pub trait CharacterWidth {
    /// Return 2 for Python East Asian F/W and 1 otherwise, or explicitly unsupported.
    fn width(&self, character: char) -> Option<usize>;
}

impl<F: Fn(char) -> Option<usize>> CharacterWidth for F {
    fn width(&self, character: char) -> Option<usize> {
        self(character)
    }
}

/// ASCII provider; non-ASCII width fails explicitly instead of using wcwidth.
pub fn ascii_width(character: char) -> Option<usize> {
    character.is_ascii().then_some(1)
}

/// JSON versus text selection, independent of ambient configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// Existing product JSON envelope.
    Json,
    /// Source text branch.
    Text,
}

impl OutputFormat {
    /// Use a truthy explicit format, otherwise the caller's already-resolved config.
    pub fn resolve(explicit: Option<&str>, config: Option<&str>) -> Result<Self, TextError> {
        match explicit.filter(|s| !s.is_empty()).or(config) {
            Some("json") => Ok(Self::Json),
            Some("text") => Ok(Self::Text),
            value => Err(TextError::UnsupportedFormat(
                value.unwrap_or("None").to_owned(),
            )),
        }
    }
}

/// Semantic source style; callers inject escape sequences instead of probing a TTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// No styling.
    Plain,
    /// `fg:grey`.
    Grey,
    /// `ansigreen` success marker.
    Success,
    /// `ansired` error marker.
    Error,
    /// `ansiyellow` warning marker.
    Warning,
    /// `ansiblue` info marker.
    Info,
    /// Fabric brand foreground.
    Fabric,
}

/// Caller-selected framing; never reads terminal capabilities or normalizes bytes.
#[derive(Clone, Copy)]
pub struct Presentation {
    /// Caller says the destination is a terminal; false suppresses injected colors.
    pub tty: bool,
    /// Return prefix/suffix for a source style (both empty for no color).
    pub color: fn(Style) -> (&'static str, &'static str),
    /// Printed record terminator, ordinarily LF, or caller-selected CRLF.
    pub newline: &'static str,
    /// Emulate the source's formatted-print fallback, including escaped text quirks.
    pub fallback: bool,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            tty: false,
            color: |_| ("", ""),
            newline: "\n",
            fallback: false,
        }
    }
}

/// Text-only flags, kept separate so existing [`crate::OutputInput`] literals survive.
#[derive(Clone, Debug)]
pub struct TextOptions<'a> {
    /// Already-selected subcommand, used for `ls`/`dir` routing.
    pub subcommand: Option<&'a str>,
    /// Force table routing and column headers.
    pub show_headers: bool,
    /// Key/value routing unless table routing has precedence.
    pub key_value: bool,
    /// Columns to truncate in priority order; mutates only a private data clone.
    pub truncate_columns: &'a [String],
    /// Injected terminal width; absent reproduces table80/truncation120 fallbacks.
    pub terminal_width: Option<usize>,
    /// Source-equivalent key formatter; defaults to the embedded Unicode provider.
    pub key_formatter: fn(&str) -> Result<String, TextError>,
    /// Source-equivalent Python str/repr formatter for JSON-domain data.
    pub value_formatter: fn(&Value) -> Result<String, TextError>,
}

impl Default for TextOptions<'_> {
    fn default() -> Self {
        Self {
            subcommand: None,
            show_headers: false,
            key_value: false,
            truncate_columns: &[],
            terminal_width: None,
            key_formatter: crate::production_unicode::key_formatter,
            value_formatter: crate::production_unicode::value_formatter,
        }
    }
}

/// Ordered text records, permitting source stdout/stderr interleaving.
///
/// Unlike a single JSON envelope, text hidden data has a separator on stderr
/// followed by values on stdout. Existing JSON [`RenderPlan`] remains unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPlan {
    /// Ordered writes; each record retains its selected stream.
    pub records: Vec<RenderPlan>,
    /// Caller-owned exit status, never applied by the library.
    pub exit_code: u8,
}

impl TextPlan {
    /// Write in source order, propagating the first I/O error without fallback/flush.
    pub fn write_to(&self, stdout: &mut impl Write, stderr: &mut impl Write) -> io::Result<()> {
        for record in &self.records {
            record.write_to(stdout, stderr)?;
        }
        Ok(())
    }
}

struct Printer {
    output: String,
    records: Vec<RenderPlan>,
    presentation: Presentation,
    key_formatter: fn(&str) -> Result<String, TextError>,
    value_formatter: fn(&Value) -> Result<String, TextError>,
}

impl Printer {
    fn new(presentation: Presentation) -> Self {
        Self {
            output: String::new(),
            records: Vec::new(),
            presentation,
            key_formatter: crate::production_unicode::key_formatter,
            value_formatter: crate::production_unicode::value_formatter,
        }
    }

    fn styled(&mut self, text: &str, style: Style) {
        let (prefix, suffix) = if self.presentation.tty && !self.presentation.fallback {
            (self.presentation.color)(style)
        } else {
            ("", "")
        };
        self.output.push_str(prefix);
        self.output.push_str(text);
        self.output.push_str(suffix);
    }

    fn line(&mut self, text: &str, style: Style) {
        self.styled(text, style);
        self.output.push_str(self.presentation.newline);
    }

    fn line_on(&mut self, text: &str, style: Style, stream: OutputStream) {
        self.save(OutputStream::Stdout, 0);
        self.line(text, style);
        self.save(stream, 0);
    }

    fn save(&mut self, stream: OutputStream, exit_code: u8) {
        if !self.output.is_empty() {
            self.records.push(RenderPlan {
                stream,
                exit_code,
                payload: std::mem::take(&mut self.output).into_bytes(),
            });
        }
    }

    fn plan(mut self, stream: OutputStream, exit_code: u8) -> TextPlan {
        self.save(stream, exit_code);
        TextPlan {
            records: self.records,
            exit_code,
        }
    }

    fn fail(&mut self, error: TextError) -> TextError {
        if self.output.is_empty() && self.records.is_empty() {
            return error;
        }
        let printer = std::mem::replace(self, Self::new(self.presentation));
        TextError::AfterOutput {
            error: Box::new(error),
            partial: Box::new(printer.plan(OutputStream::Stdout, 1)),
        }
    }

    fn formatted(&mut self, prefix: &str, marker: &str, body: &str, style: Style) {
        if self.presentation.fallback {
            self.line(
                &html_escape(body.strip_prefix(' ').unwrap_or(body)),
                Style::Plain,
            );
        } else {
            self.output.push_str(prefix);
            self.styled(marker, style);
            self.line(body, Style::Plain);
        }
    }
}

/// Render an envelope in the selected format; JSON keeps its existing exact API.
///
/// Failure text uses the shared error renderer, not success data/message routing.
/// Error fields are already normalized; empty messages and trailing periods remain.
pub fn render(
    envelope: &OutputEnvelope,
    format: OutputFormat,
    options: &TextOptions<'_>,
    widths: &impl CharacterWidth,
    presentation: Presentation,
) -> Result<TextPlan, TextError> {
    if format == OutputFormat::Json {
        let plan = envelope.render(Indent::Four)?;
        return Ok(TextPlan {
            exit_code: plan.exit_code,
            records: vec![plan],
        });
    }
    let result = envelope.result();
    if envelope.status == OutputStatus::Failure {
        return Ok(render_error(
            &CommandError {
                message: result.message().unwrap_or_default().to_owned(),
                code: result
                    .error_code()
                    .expect("failure envelope has a normalized error code")
                    .to_owned(),
            },
            envelope.command.as_deref(),
            presentation,
        ));
    }
    if result.data().is_none() && result.hidden_data().is_none() && result.message().is_none() {
        return Err(TextError::InvalidResult);
    }
    let mut printer = Printer::new(presentation);
    printer.key_formatter = options.key_formatter;
    printer.value_formatter = options.value_formatter;
    if let Some(data) = result.data().filter(|data| !data.is_empty()) {
        let mut data = data.to_vec();
        if !options.truncate_columns.is_empty() && data[0].is_object() {
            truncate(
                &mut data,
                options.truncate_columns,
                options.terminal_width.unwrap_or(120),
                options.value_formatter,
            )?;
        }
        let table = matches!(envelope.command.as_deref(), Some("ls" | "dir"))
            || matches!(options.subcommand, Some("ls" | "dir"))
            || options.show_headers;
        if table && data[0].as_object().is_some_and(|row| !row.is_empty()) {
            let fields: Vec<_> = data[0].as_object().unwrap().keys().cloned().collect();
            table_lines(
                &data,
                &fields,
                fields.len() > 1 || options.show_headers,
                options.terminal_width.unwrap_or(80),
                widths,
                &mut printer,
            )
            .map_err(|error| printer.fail(error))?;
        } else if !table && options.key_value {
            key_value_lines(&data, &mut printer).map_err(|error| printer.fail(error))?;
        } else {
            raw_lines(&data, &mut printer).map_err(|error| printer.fail(error))?;
        }
    }
    if let Some(hidden) = result.hidden_data().filter(|data| !data.is_empty()) {
        printer.line_on(
            "------------------------------",
            Style::Grey,
            OutputStream::Stderr,
        );
        for value in hidden {
            printer.line(value, Style::Grey);
        }
    }
    if let Some(message) = result.message().filter(|message| !message.is_empty()) {
        printer.formatted("\n", "*", &format!(" {message}\n"), Style::Success);
    }
    Ok(printer.plan(
        OutputStream::Stdout,
        envelope.render(Indent::Four)?.exit_code,
    ))
}

fn raw_lines(data: &[Value], printer: &mut Printer) -> Result<(), TextError> {
    if data.first().is_some_and(Value::is_object) {
        let value = if data.len() == 1 {
            data[0].clone()
        } else {
            Value::Array(data.to_vec())
        };
        printer.line(&encoding::encode(&value, Indent::Two), Style::Grey);
    } else {
        for value in data {
            printer.line(&(printer.value_formatter)(value)?, Style::Grey);
        }
    }
    Ok(())
}

fn row<'a>(value: &'a Value, method: &'static str) -> Result<&'a Map<String, Value>, TextError> {
    value.as_object().ok_or(TextError::InvalidRow {
        kind: python_kind(value),
        method,
    })
}

fn python_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn get(
    row: &Map<String, Value>,
    field: &str,
    formatter: fn(&Value) -> Result<String, TextError>,
) -> Result<String, TextError> {
    row.get(field)
        .map(formatter)
        .transpose()
        .map(|v| v.unwrap_or_default())
}

fn visual(text: &str, widths: &impl CharacterWidth) -> Result<usize, TextError> {
    text.chars()
        .map(|c| widths.width(c).ok_or(TextError::MissingWidth(c as u32)))
        .sum()
}

fn source_trim_end(value: &str) -> Result<&str, TextError> {
    let properties = crate::production_unicode::width_provider()?;
    Ok(value.trim_end_matches(|character| properties.properties(character).whitespace))
}

fn wrap(text: &str, width: usize, widths: &impl CharacterWidth) -> Result<Vec<String>, TextError> {
    if width == 0 {
        return Ok(vec![text.to_owned()]);
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for c in text.chars() {
        let size = visual(&c.to_string(), widths)?;
        if current_width + size > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(c);
        current_width += size;
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    Ok(lines)
}

fn padded_rows(
    cells: Vec<Vec<String>>,
    sizes: &[usize],
    headers: bool,
    widths: &impl CharacterWidth,
) -> Result<Vec<String>, TextError> {
    let height = cells.iter().map(Vec::len).max().unwrap_or(1);
    (0..height)
        .map(|index| {
            let mut line = String::new();
            for (cell, &size) in cells.iter().zip(sizes) {
                let value = cell.get(index).map(String::as_str).unwrap_or("");
                line.push_str(value);
                let length = if headers {
                    value.chars().count()
                } else {
                    visual(value, widths)?
                };
                line.extend(std::iter::repeat_n(' ', size.saturating_sub(length) + 1));
            }
            Ok(source_trim_end(&line)?.to_owned())
        })
        .collect()
}

fn table_lines(
    entries: &[Value],
    fields: &[String],
    header: bool,
    terminal: usize,
    widths: &impl CharacterWidth,
    printer: &mut Printer,
) -> Result<(), TextError> {
    let empty = [Value::Object(Map::new())];
    let entries = if entries.is_empty() {
        &empty[..]
    } else {
        entries
    };
    let mut sizes = Vec::new();
    for field in fields {
        let mut size = if header { visual(field, widths)? } else { 0 };
        for entry in entries {
            let value = get(row(entry, "get")?, field, printer.value_formatter)?;
            size = size.max(if header {
                visual(&value, widths)?
            } else {
                value.chars().count()
            });
        }
        sizes.push(size + 2);
    }
    let separators = sizes.len().saturating_sub(1);
    if !sizes.is_empty() && sizes.iter().sum::<usize>() + separators > terminal {
        let available = terminal.saturating_sub(separators);
        let minimum = (available / sizes.len()).clamp(1, 8);
        if available > 0 {
            let total = sizes.iter().sum::<usize>();
            for size in &mut sizes {
                *size = minimum.max((*size as f64 * available as f64 / total as f64) as usize);
            }
            while sizes.iter().sum::<usize>() + separators > terminal {
                let index = (0..sizes.len())
                    .max_by_key(|&i| (sizes[i], std::cmp::Reverse(i)))
                    .unwrap();
                if sizes[index] <= minimum {
                    break;
                }
                sizes[index] -= 1;
            }
            if sizes.iter().sum::<usize>() + separators > terminal {
                let count = sizes.len();
                for (index, size) in sizes.iter_mut().enumerate() {
                    *size = 1.max(available / count + usize::from(index < available % count));
                }
            }
        }
    }
    if header {
        let cells = fields
            .iter()
            .zip(&sizes)
            .map(|(field, &size)| wrap(field, size, widths))
            .collect::<Result<Vec<_>, _>>()?;
        let lines = padded_rows(cells, &sizes, true, widths)?;
        for line in &lines {
            printer.line(line, Style::Grey);
        }
        let separator = if let Some(first) = lines.first() {
            visual(first, widths)?
        } else {
            sizes.iter().sum::<usize>() + separators
        };
        printer.line(&"-".repeat(separator), Style::Grey);
    }
    for entry in entries {
        let entry = row(entry, "get")?;
        let cells = fields
            .iter()
            .zip(&sizes)
            .map(|(field, &size)| wrap(&get(entry, field, printer.value_formatter)?, size, widths))
            .collect::<Result<Vec<_>, _>>()?;
        for line in padded_rows(cells, &sizes, false, widths)? {
            printer.line(&line, Style::Grey);
        }
    }
    Ok(())
}

/// Render Python's standalone table helper, including empty-list placeholder rows.
pub fn render_table(
    entries: &Value,
    fields: &[String],
    header: bool,
    terminal: usize,
    widths: &impl CharacterWidth,
    presentation: Presentation,
) -> Result<TextPlan, TextError> {
    let entries = match entries {
        Value::Object(_) => vec![entries.clone()],
        Value::Array(values) => values.clone(),
        _ => return Err(TextError::InvalidTableEntries),
    };
    let mut printer = Printer::new(presentation);
    table_lines(&entries, fields, header, terminal, widths, &mut printer)
        .map_err(|error| printer.fail(error))?;
    Ok(printer.plan(OutputStream::Stdout, 0))
}

/// Standalone key/value helper, retaining its own invalid-entries error.
pub fn render_key_values(
    entries: &Value,
    key_formatter: fn(&str) -> Result<String, TextError>,
    value_formatter: fn(&Value) -> Result<String, TextError>,
    presentation: Presentation,
) -> Result<TextPlan, TextError> {
    let entries = match entries {
        Value::Object(_) => vec![entries.clone()],
        Value::Array(values) => values.clone(),
        _ => return Err(TextError::InvalidEntries),
    };
    let mut printer = Printer::new(presentation);
    printer.key_formatter = key_formatter;
    printer.value_formatter = value_formatter;
    key_value_lines(&entries, &mut printer).map_err(|error| printer.fail(error))?;
    Ok(printer.plan(OutputStream::Stdout, 0))
}

fn truncate(
    items: &mut [Value],
    columns: &[String],
    terminal: usize,
    formatter: fn(&Value) -> Result<String, TextError>,
) -> Result<(), TextError> {
    let fields: Vec<_> = row(&items[0], "keys")?.keys().cloned().collect();
    let mut sizes = Vec::new();
    for field in &fields {
        let mut size = field.chars().count();
        for item in items.iter() {
            size = size.max(get(row(item, "get")?, field, formatter)?.chars().count());
        }
        sizes.push(size);
    }
    let mut shorten = |column: &str, size: usize| -> Result<(), TextError> {
        for item in items.iter_mut() {
            let value = get(row(item, "get")?, column, formatter)?;
            if value.chars().count() > size {
                let prefix = value
                    .chars()
                    .take(size.saturating_sub(1))
                    .collect::<String>();
                let shortened = source_trim_end(&prefix)?.to_owned() + "\u{2026}";
                item.as_object_mut()
                    .unwrap()
                    .insert(column.to_owned(), Value::String(shortened));
            }
        }
        Ok(())
    };
    for column in columns {
        if let Some(index) = fields.iter().position(|f| f == column)
            && sizes[index] > 50
        {
            sizes[index] = 50;
            shorten(column, 50)?;
        }
    }
    for column in columns {
        let Some(index) = fields.iter().position(|f| f == column) else {
            continue;
        };
        let total = sizes.iter().sum::<usize>() as isize + 3 * fields.len() as isize - 1;
        let overflow = total - terminal as isize;
        if overflow <= 0 {
            break;
        }
        sizes[index] = (sizes[index] as isize - overflow).max(20) as usize;
        shorten(column, sizes[index])?;
    }
    Ok(())
}

fn key_value_lines(entries: &[Value], printer: &mut Printer) -> Result<(), TextError> {
    for (index, entry) in entries.iter().enumerate() {
        for (key, value) in row(entry, "items")? {
            printer.line(
                &format!(
                    "{}: {}",
                    (printer.key_formatter)(key)?,
                    (printer.value_formatter)(value)?
                ),
                Style::Grey,
            );
        }
        if index + 1 < entries.len() {
            printer.line("", Style::Grey);
        }
    }
    Ok(())
}

/// Validate/format ASCII source keys; non-ASCII casing requires explicit future data.
pub fn title_key(key: &str) -> Result<String, TextError> {
    if !key.is_ascii() {
        return Err(TextError::UnicodeKey(key.to_owned()));
    }
    let stripped = key.replace(['_', ' '], "");
    if stripped.is_empty()
        || !stripped.chars().all(|c| c.is_alphanumeric())
        || (key.contains(' ') && key.contains('_'))
        || (!key.contains('_') && key.chars().skip(1).any(|c| c.is_uppercase()))
    {
        return Err(TextError::InvalidKey(key.to_owned()));
    }
    let mut title = String::new();
    let mut boundary = true;
    for c in key.replace('_', " ").chars() {
        title.push(if boundary {
            c.to_ascii_uppercase()
        } else {
            c.to_ascii_lowercase()
        });
        boundary = !c.is_alphabetic();
    }
    Ok(title
        .trim()
        .replace("Id", "ID")
        .replace("Cli", "CLI")
        .replace("Powerbi", "PowerBI"))
}

/// Python str/repr for JSON data, with explicit Unicode nested-repr limitations.
///
/// Top-level strings are returned unchanged. ASCII nested strings reproduce
/// Python quote selection and escaping; nested non-ASCII strings require a
/// caller-supplied `TextOptions::value_formatter` with pinned Python printability.
/// No exotic runtime coercion or implicit Unicode property approximation occurs.
pub fn python_string(value: &Value) -> Result<String, TextError> {
    Ok(match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(value) => value.clone(),
        Value::Number(_) => encoding::encode_default(value),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| Ok(format!("{}: {}", string_repr(key)?, python_repr(value)?)))
                .collect::<Result<Vec<_>, TextError>>()?
                .join(", ")
        ),
    })
}

fn python_repr(value: &Value) -> Result<String, TextError> {
    match value {
        Value::String(text) => string_repr(text),
        _ => python_string(value),
    }
}

fn string_repr(value: &str) -> Result<String, TextError> {
    if !value.is_ascii() {
        return Err(TextError::UnicodeRepr(value.to_owned()));
    }
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut text = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => text.push_str("\\\\"),
            '\t' => text.push_str("\\t"),
            '\r' => text.push_str("\\r"),
            '\n' => text.push_str("\\n"),
            c if c == quote => {
                text.push('\\');
                text.push(c);
            }
            c if c.is_ascii_control() => {
                use std::fmt::Write;
                write!(text, "\\x{:02x}", c as u32).expect("writing String cannot fail");
            }
            _ => text.push(c),
        }
    }
    text.push(quote);
    Ok(text)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Source UI notices with their individual stream and punctuation rules.
#[derive(Clone, Copy, Debug)]
pub enum Notice<'a> {
    /// `print_done`, default stdout, preserving supplied periods/newlines.
    Done(&'a str),
    /// `print_warning`, default stderr, stripping trailing periods.
    Warning(&'a str),
    /// `print_info`, default stderr, stripping trailing periods.
    Info(&'a str),
    /// `print_progress`, grey stderr; empty progress omits the percentage suffix.
    Progress {
        /// Display text.
        text: &'a str,
        /// Already-selected source progress string.
        progress: Option<&'a str>,
    },
}

/// Render a notice without prompting or discovering a terminal.
pub fn render_notice(
    notice: Notice<'_>,
    command: Option<&str>,
    presentation: Presentation,
) -> TextPlan {
    let mut printer = Printer::new(presentation);
    let command = command
        .filter(|s| !s.is_empty())
        .map(|s| format!("{s}: "))
        .unwrap_or_default();
    let stream = match notice {
        Notice::Done(text) => {
            printer.formatted("\n", "*", &format!(" {text}"), Style::Success);
            OutputStream::Stdout
        }
        Notice::Warning(text) | Notice::Info(text) => {
            let (marker, style) = if matches!(notice, Notice::Warning(_)) {
                ("!", Style::Warning)
            } else {
                ("*", Style::Info)
            };
            if presentation.fallback {
                printer.line(&html_escape(text.trim_end_matches('.')), Style::Plain);
            } else {
                printer.formatted(
                    "",
                    marker,
                    &format!(" {command}{}", text.trim_end_matches('.')),
                    style,
                );
            }
            OutputStream::Stderr
        }
        Notice::Progress { text, progress } => {
            let progress = progress
                .filter(|s| !s.is_empty())
                .map(|s| format!(": {s}%"))
                .unwrap_or_default();
            printer.line(&format!("\u{221f} {text}{progress}"), Style::Grey);
            OutputStream::Stderr
        }
    };
    printer.plan(stream, 0)
}

/// Render a source `FabricCLIError` as text on stdout, preserving command prefixes.
///
/// Request-ID/details formatting is caller-provided text, not response parsing.
pub fn render_error(
    error: &CommandError,
    command: Option<&str>,
    presentation: Presentation,
) -> TextPlan {
    render_decorated_error(error, command, ErrorDecoration::Plain, presentation)
}

/// Already-selected Fabric API detail fields, without SDK error response parsing.
#[derive(Clone, Copy, Debug)]
pub struct ErrorDetail<'a> {
    /// Missing source fields display Python `None`.
    pub code: Option<&'a str>,
    /// Missing source fields display Python `None`.
    pub message: Option<&'a str>,
}

/// Source text-only API error decorations; never included in the JSON envelope.
#[derive(Clone, Copy, Debug)]
pub enum ErrorDecoration<'a> {
    /// Base FabricCLIError.
    Plain,
    /// FabricAPIError always emits request ID, including a missing value as None.
    Fabric {
        /// Already-extracted request ID.
        request_id: Option<&'a str>,
        /// Verbose details.
        details: &'a [ErrorDetail<'a>],
        /// Source `formatted_message(verbose=True)` selection.
        verbose: bool,
    },
    /// Already-extracted OneLake fields, emitted only when truthy.
    Onelake {
        /// Already-extracted timestamp.
        timestamp: Option<&'a str>,
        /// Already-extracted request ID.
        request_id: Option<&'a str>,
    },
    /// Azure emits request ID only when truthy.
    Azure {
        /// Already-extracted request ID.
        request_id: Option<&'a str>,
    },
}

/// Render error text and source API decoration branches without service parsing.
pub fn render_decorated_error(
    error: &CommandError,
    command: Option<&str>,
    decoration: ErrorDecoration<'_>,
    presentation: Presentation,
) -> TextPlan {
    let mut printer = Printer::new(presentation);
    let message = format!("[{}] {}", error.code, error.message);
    let mut lines = Vec::new();
    match decoration {
        ErrorDecoration::Plain => {}
        ErrorDecoration::Fabric {
            request_id,
            details,
            verbose,
        } => {
            if verbose && !details.is_empty() {
                lines.push(
                    details
                        .iter()
                        .map(|detail| {
                            format!(
                                "\u{221f} [{}] {}",
                                detail.code.unwrap_or("None"),
                                detail.message.unwrap_or("None")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            lines.push(format!(
                "\u{221f} Request Id: {}",
                request_id.unwrap_or("None")
            ));
        }
        ErrorDecoration::Onelake {
            timestamp,
            request_id,
        } => {
            if let Some(timestamp) = timestamp.filter(|s| !s.is_empty()) {
                lines.push(format!("\u{221f} Timestamp: {timestamp}"));
            }
            if let Some(request_id) = request_id.filter(|s| !s.is_empty()) {
                lines.push(format!("\u{221f} Request Id: {request_id}"));
            }
        }
        ErrorDecoration::Azure { request_id } => {
            if let Some(request_id) = request_id.filter(|s| !s.is_empty()) {
                lines.push(format!("\u{221f} Request Id: {request_id}"));
            }
        }
    }
    if presentation.fallback {
        // Source fallback receives already-escaped text plus literal grey markup.
        let mut fallback = html_escape(&message);
        for line in &lines {
            fallback.push_str(&format!("\n<grey>{}</grey>", html_escape(line)));
        }
        printer.line(&fallback, Style::Plain);
    } else {
        let command = command
            .filter(|s| !s.is_empty())
            .map(|s| format!("{s}: "))
            .unwrap_or_default();
        printer.styled("x", Style::Error);
        printer.output.push_str(&format!(" {command}{message}"));
        for line in &lines {
            printer.output.push('\n');
            printer.styled(line, Style::Grey);
        }
        printer.output.push_str(presentation.newline);
    }
    printer.plan(
        OutputStream::Stdout,
        if error.code == "Unauthorized" { 4 } else { 1 },
    )
}

/// Render the source's explicit version string; version discovery belongs to caller.
pub fn render_version(version: &str, presentation: Presentation) -> TextPlan {
    let mut printer = Printer::new(presentation);
    printer.line(&format!("fab version {version}"), Style::Plain);
    printer.line("https://aka.ms/fabric-cli/release-notes", Style::Plain);
    printer.plan(OutputStream::Stdout, 0)
}

/// Caller-ordered command help category.
#[derive(Clone, Debug)]
pub struct HelpCategory<'a> {
    /// Display name.
    pub name: &'a str,
    /// Ordered command name and description pairs.
    pub commands: &'a [(&'a str, &'a str)],
}

/// Render `display_help` without a parser, registry, terminal or environment lookup.
pub fn render_help(
    categories: &[HelpCategory<'_>],
    header: Option<&str>,
    presentation: Presentation,
) -> Result<TextPlan, TextError> {
    let mut printer = Printer::new(presentation);
    if categories.is_empty() {
        printer.line("No commands available.", Style::Plain);
        return Ok(printer.plan(OutputStream::Stdout, 0));
    }
    if let Some(header) = header.filter(|s| !s.is_empty()) {
        printer.line(&format!("{header} \n"), Style::Plain);
    } else {
        printer.line(
            "Work seamlessly with Fabric from the command line.\n",
            Style::Plain,
        );
        printer.line("Usage: fab <command> <subcommand> [flags]\n", Style::Plain);
    }
    let maximum = categories
        .iter()
        .flat_map(|category| category.commands.iter())
        .map(|(command, _)| command.chars().count())
        .max()
        .ok_or(TextError::EmptyHelp)
        .map_err(|error| printer.fail(error))?;
    for category in categories {
        printer.line(&format!("{}:", category.name), Style::Plain);
        for &(name, description) in category.commands {
            printer.line(
                &format!(
                    "  {name}{}: {description}",
                    " ".repeat(maximum.saturating_sub(name.chars().count()))
                ),
                Style::Plain,
            );
        }
        printer.line("", Style::Plain);
    }
    printer.line("Learn More:", Style::Plain);
    printer.line(
        "  Use `fab <command> <subcommand> --help` for more information about a command.",
        Style::Plain,
    );
    printer.line(
        "  Use `fab config set mode interactive` to enable interactive mode.",
        Style::Plain,
    );
    printer.line(
        "  Read the docs at https://aka.ms/fabric-cli.\n",
        Style::Plain,
    );
    Ok(printer.plan(OutputStream::Stdout, 0))
}

/// Source plain/grey/Fabric print boundary with explicit style and destination.
///
/// This also lets a caller select non-default destinations for done/notice output
/// by retaining source intent without probing globals. Actual I/O failures remain
/// errors of `TextPlan::write_to`, not reasons for a second fallback write.
pub fn render_line(
    text: &str,
    style: Style,
    stream: OutputStream,
    presentation: Presentation,
) -> TextPlan {
    let mut printer = Printer::new(presentation);
    printer.line(text, style);
    printer.plan(stream, 0)
}
