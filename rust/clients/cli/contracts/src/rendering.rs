// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CommandError, CommandValue};
use fabric_cli_output::{
    OutputEnvelope, OutputInput, OutputStream, Timestamp,
    text::{
        self, CharacterWidth, OutputFormat, Presentation, Style, TextError, TextOptions, TextPlan,
    },
};

/// Source output routing supplied by a family handler, not a second parser/host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FamilySurface {
    /// ACL LS/dir projects columns and always requests headers for nonempty data.
    AclList,
    /// ACL GET or internal metadata GET: ordinary source result routing.
    Get,
    /// Local label definitions request headers, including the empty-table branch.
    LocalLabels,
    /// ACL/label mutations produce a source success message or no output.
    Mutation,
    /// Catalog FIND uses headers and short-mode truncation with a trailing blank.
    Find {
        /// Long output disables the source text truncation priorities.
        long: bool,
    },
    /// Raw API status/header/text data uses ordinary source routing.
    Api,
}

/// Caller-selected output format/terminal behavior; no config or TTY is probed.
#[derive(Clone)]
pub struct FamilyPresentation<'a> {
    /// Already resolved from explicit arguments or caller-owned config.
    pub format: OutputFormat,
    /// Family-specific source output routing.
    pub surface: FamilySurface,
    /// Source table/truncation width, or its existing shared fallback.
    pub terminal_width: Option<usize>,
    /// Caller-selected line endings, colors, TTY and formatted-print fallback.
    pub presentation: Presentation,
    /// Optional source-equivalent Unicode key/repr formatters and low-level
    /// flags; family header/truncation routing takes precedence over these flags.
    pub text: TextOptions<'a>,
}

impl FamilyPresentation<'_> {
    /// Selects plain LF/non-TTY output without reading any ambient configuration.
    pub fn new(format: OutputFormat, surface: FamilySurface) -> Self {
        Self {
            format,
            surface,
            terminal_width: None,
            presentation: Presentation::default(),
            text: TextOptions::default(),
        }
    }
}

fn lines(plan: &mut TextPlan, diagnostics: &[String], presentation: Presentation) {
    for line in diagnostics {
        plan.records.extend(
            text::render_line(line, Style::Grey, OutputStream::Stderr, presentation).records,
        );
    }
}

fn append(plan: &mut TextPlan, rendered: Result<TextPlan, TextError>) -> Result<(), TextError> {
    match rendered {
        Ok(rendered) => {
            plan.records.extend(rendered.records);
            plan.exit_code = rendered.exit_code;
            Ok(())
        }
        Err(error) if plan.records.is_empty() => Err(error),
        Err(TextError::AfterOutput { error, partial }) => {
            plan.records.extend(partial.records);
            Err(TextError::AfterOutput {
                error,
                partial: Box::new(plan.clone()),
            })
        }
        Err(error) => Err(TextError::AfterOutput {
            error: Box::new(error),
            partial: Box::new(plan.clone()),
        }),
    }
}

/// Composes actual family data with the released shared JSON/text renderer.
/// Warnings remain caller-logger observations; diagnostics preserve source order.
/// FIND's final blank follows its output, not the summary preceding that output.
/// A source-silent declined/empty result performs no output-rendering call.
pub fn render_family_output(
    value: &CommandValue,
    command: &str,
    timestamp: Timestamp,
    options: &FamilyPresentation<'_>,
    widths: &impl CharacterWidth,
) -> Result<TextPlan, TextError> {
    let mut plan = TextPlan {
        records: Vec::new(),
        exit_code: 0,
    };
    let emits = value.data.is_some() || value.message.is_some();
    let trailing = usize::from(
        emits
            && matches!(options.surface, FamilySurface::Find { .. })
            && value.diagnostics.last().is_some_and(String::is_empty),
    );
    let split = value.diagnostics.len() - trailing;
    lines(&mut plan, &value.diagnostics[..split], options.presentation);
    if emits {
        let columns = if options.surface == (FamilySurface::Find { long: false }) {
            vec!["description".into(), "workspace".into(), "name".into()]
        } else {
            Vec::new()
        };
        let text_options = TextOptions {
            show_headers: matches!(
                options.surface,
                FamilySurface::AclList | FamilySurface::LocalLabels | FamilySurface::Find { .. }
            ),
            truncate_columns: &columns,
            terminal_width: options.terminal_width,
            ..options.text.clone()
        };
        let envelope = OutputEnvelope::new(
            timestamp,
            OutputInput {
                command: Some(command.into()),
                data: value.data.clone(),
                message: value.message.clone(),
                ..OutputInput::default()
            },
        );
        append(
            &mut plan,
            text::render(
                &envelope,
                options.format,
                &text_options,
                widths,
                options.presentation,
            ),
        )?;
    }
    lines(&mut plan, &value.diagnostics[split..], options.presentation);
    Ok(plan)
}

/// Renders family failure through the existing shared error/envelope policy.
/// Source-handled OneLake security errors retain Failure data but exit zero,
/// and their guidance follows the error. Ordinary pre-failure diagnostics precede it.
pub fn render_family_error(
    error: &CommandError,
    command: &str,
    timestamp: Timestamp,
    options: &FamilyPresentation<'_>,
) -> Result<TextPlan, TextError> {
    let mut plan = TextPlan {
        records: Vec::new(),
        exit_code: 0,
    };
    if !error.handled {
        lines(&mut plan, &error.diagnostics, options.presentation);
    }
    let source =
        fabric_cli_output::CommandError::new(Some(error.message.clone()), Some(error.code.clone()));
    let rendered = match options.format {
        OutputFormat::Text => text::render_error(&source, Some(command), options.presentation),
        OutputFormat::Json => TextPlan {
            records: vec![
                source
                    .envelope(timestamp, Some(command.into()))
                    .render(fabric_cli_output::Indent::Four)?,
            ],
            exit_code: if error.code == "Unauthorized" { 4 } else { 1 },
        },
    };
    plan.records.extend(rendered.records);
    plan.exit_code = if error.handled {
        0
    } else if error.code == "Unauthorized" {
        4
    } else {
        1
    };
    if error.handled {
        lines(&mut plan, &error.diagnostics, options.presentation);
    }
    Ok(plan)
}
