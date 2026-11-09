// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in BPA command adapter over the SDK's single analysis contract.
//!
//! The caller owns files, cancellation, backend and output. Remote path analysis
//! remains explicitly unavailable; local BIM input is never uploaded or applied.

use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_sdk_semantic_model::{
    AnalysisBackend, AnalysisError, AnalysisOptions, AnalysisOutcome, AnalysisReport,
    AnalysisRequest, DEFAULT_MAX_BYTES, Severity, analyze,
};
use serde_json::Value;
use std::{io::Write, path::PathBuf, sync::atomic::AtomicBool};

/// One parsed read-only analysis request.
#[derive(Clone, Debug)]
pub struct AnalyzeCommand {
    /// Explicit inherited format override; this bounded adapter advertises JSON.
    pub output_format: Option<String>,
    /// Remote target, currently diagnosed as unsupported before file access.
    pub path: Option<String>,
    /// Explicit local model.bim input, not a remote resource path.
    pub input: Option<PathBuf>,
    /// Caller-selected trusted native runtime library; not model-controlled data.
    pub hostfxr: Option<PathBuf>,
    /// Explicit managed deployment directory; otherwise Cargo development output.
    pub managed_component: Option<PathBuf>,
    /// Explicit local JSON evidence; omitted inputs are not known-empty evidence.
    pub evidence: Option<PathBuf>,
    /// Minimum displayed finding severity.
    pub severity: Severity,
    /// Fail a complete analysis with findings meeting the threshold.
    pub strict: bool,
    /// Request extended-statistics rules, using explicit supplied evidence only.
    pub extended: bool,
    /// Skip dependency collection while retaining unavailable-rule evidence.
    pub without_dependencies: bool,
    /// Recognized only to reject unsupported mutation intent before work.
    pub fix: bool,
}

/// Construct the bounded artifact facade without reading files or configuration.
pub fn command() -> Command {
    Command::new("artifact")
        .about("Opt-in artifact capabilities")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("semantic-model")
                .about("Semantic-model capabilities")
                .arg_required_else_help(true)
                .subcommand(
                    Command::new("analyze")
                        .about("Run pinned Semantic Link Labs BPA against a local BIM snapshot")
                        .arg(
                            Arg::new("path")
                                .required_unless_present("input")
                                .conflicts_with("input")
                                .help("Remote model path; remote analysis is not implemented"),
                        )
                        .arg(
                            Arg::new("input")
                                .long("input")
                                .value_name("MODEL.BIM")
                                .help("Analyze this explicit local BIM file without remote access"),
                        )
                        .arg(
                            Arg::new("hostfxr")
                                .long("hostfxr")
                                .value_name("LIBRARY")
                                .requires("input")
                                .help("Absolute trusted hostfxr library path for optional .NET execution"),
                        )
                        .arg(
                            Arg::new("managed_component")
                                .long("managed-component")
                                .value_name("DIRECTORY")
                                .requires("hostfxr")
                                .help("Trusted deployed managed component; otherwise use Cargo build output"),
                        )
                        .arg(
                            Arg::new("evidence")
                                .long("evidence")
                                .value_name("JSON")
                                .requires("input")
                                .help("Explicit rule evidence; no live collection is performed"),
                        )
                        .arg(
                            Arg::new("severity")
                                .long("severity")
                                .value_parser(["info", "warning", "error"])
                                .default_value("info"),
                        )
                        .arg(Arg::new("strict").long("strict").action(ArgAction::SetTrue))
                        .arg(Arg::new("extended").long("extended").action(ArgAction::SetTrue))
                        .arg(
                            Arg::new("without_dependencies")
                                .long("without-dependencies")
                                .action(ArgAction::SetTrue),
                        )
                        .arg(
                            Arg::new("fix")
                                .long("fix")
                                .action(ArgAction::SetTrue)
                                .help("Unsupported: BPA analysis never changes model definitions"),
                        ),
                ),
        )
}

impl AnalyzeCommand {
    /// Parse only this adapter's exact nested command.
    pub fn from_matches(matches: &ArgMatches) -> Option<Self> {
        let (_, artifact) = matches
            .subcommand()
            .filter(|(name, _)| *name == "artifact")?;
        let (_, model) = artifact
            .subcommand()
            .filter(|(name, _)| *name == "semantic-model")?;
        let (_, values) = model.subcommand().filter(|(name, _)| *name == "analyze")?;
        Some(Self {
            output_format: matches
                .try_get_one::<String>("output_format")
                .ok()
                .flatten()
                .cloned(),
            path: values.get_one::<String>("path").cloned(),
            input: values.get_one::<String>("input").map(PathBuf::from),
            hostfxr: values.get_one::<String>("hostfxr").map(PathBuf::from),
            managed_component: values
                .get_one::<String>("managed_component")
                .map(PathBuf::from),
            evidence: values.get_one::<String>("evidence").map(PathBuf::from),
            severity: match values.get_one::<String>("severity").map(String::as_str) {
                Some("warning") => Severity::Warning,
                Some("error") => Severity::Error,
                _ => Severity::Info,
            },
            strict: values.get_flag("strict"),
            extended: values.get_flag("extended"),
            without_dependencies: values.get_flag("without_dependencies"),
            fix: values.get_flag("fix"),
        })
    }

    /// Validate supported effects and targets before any file/backend call.
    ///
    /// # Errors
    /// Remote targets and mutation intent are not silently converted to local work.
    pub fn preflight(&self) -> Result<(), CommandError> {
        if self
            .output_format
            .as_deref()
            .is_some_and(|format| format != "json")
        {
            return Err(CommandError::Unsupported(
                "The BPA preview supports JSON reports only.",
            ));
        }
        if self.fix {
            return Err(CommandError::Unsupported(
                "BPA fixes are not implemented. Analysis never changes a model.",
            ));
        }
        if self.path.is_some() {
            return Err(CommandError::Unsupported(
                "Remote model analysis is not implemented. Use an explicit local BIM snapshot with --input.",
            ));
        }
        if self.input.is_none() {
            return Err(CommandError::InvalidInput);
        }
        Ok(())
    }
}

/// Caller-owned bounded local-input access.
pub trait AnalysisFiles {
    /// Read one explicitly selected regular file within the supplied byte budget.
    ///
    /// # Errors
    /// Missing, unreadable or oversized files must fail, not return empty content.
    fn read(&mut self, path: &std::path::Path, max_bytes: usize) -> Result<String, CommandError>;
}

/// Command-level failures with sanitized diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// Malformed local input or command.
    #[error(
        "The analysis input is invalid; supply a bounded UTF-8 BIM database and JSON object evidence."
    )]
    InvalidInput,
    /// Local input could not be read.
    #[error("The explicitly selected analysis file could not be read.")]
    InputRead,
    /// A finite input limit was reached.
    #[error("The analysis input exceeded its byte limit.")]
    Limit,
    /// An unavailable target or mutation was requested.
    #[error("{0}")]
    Unsupported(&'static str),
    /// The SDK rejected the analysis or its report.
    #[error(transparent)]
    Analysis(#[from] AnalysisError),
}

impl CommandError {
    /// Stable error code, independent of input contents.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput | Self::Analysis(AnalysisError::InvalidInput) => "InvalidInput",
            Self::InputRead => "InputRead",
            Self::Limit | Self::Analysis(AnalysisError::Limit) => "BudgetExceeded",
            Self::Unsupported(_) | Self::Analysis(AnalysisError::Unsupported) => {
                "UnsupportedCapability"
            }
            Self::Analysis(AnalysisError::Cancelled) => "Cancelled",
            Self::Analysis(AnalysisError::InvalidResponse) => "InvalidResponse",
            Self::Analysis(AnalysisError::Backend | AnalysisError::Binding(_)) => {
                "AnalysisBackendFailed"
            }
        }
    }
}

/// Activate only caller-selected trusted native and managed code.
///
/// # Errors
/// An absent runtime location fails without environment/PATH discovery.
#[cfg(feature = "dotnet")]
pub fn native_backend(command: &AnalyzeCommand) -> Result<impl AnalysisBackend, CommandError> {
    command.preflight()?;
    let hostfxr = command.hostfxr.as_ref().ok_or(CommandError::Unsupported(
        "Supply --hostfxr with an absolute trusted .NET runtime library path.",
    ))?;
    if !hostfxr.is_absolute()
        || command
            .managed_component
            .as_ref()
            .is_some_and(|component| !component.is_absolute())
    {
        return Err(CommandError::InvalidInput);
    }
    Ok(ConfiguredBackend {
        hostfxr: hostfxr.clone(),
        component: command.managed_component.clone(),
    })
}

#[cfg(feature = "dotnet")]
struct ConfiguredBackend {
    hostfxr: PathBuf,
    component: Option<PathBuf>,
}

#[cfg(feature = "dotnet")]
impl AnalysisBackend for ConfiguredBackend {
    fn analyze(&self, request: &[u8], max_response_bytes: usize) -> Result<Vec<u8>, AnalysisError> {
        let backend = match &self.component {
            Some(component) => {
                fabric_sdk_semantic_model::DotnetBackend::new(&self.hostfxr, component)
            }
            None => fabric_sdk_semantic_model::DotnetBackend::bundled(&self.hostfxr),
        }?;
        backend.analyze(request, max_response_bytes)
    }
}

/// Execute local analysis through the same SDK contract used by direct consumers.
///
/// # Errors
/// Validation, input, cancellation and managed failures remain explicit.
pub fn execute(
    command: &AnalyzeCommand,
    files: &mut impl AnalysisFiles,
    backend: &impl AnalysisBackend,
    cancelled: &AtomicBool,
) -> Result<AnalysisReport, CommandError> {
    command.preflight()?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(AnalysisError::Cancelled.into());
    }
    let input = command.input.as_ref().ok_or(CommandError::InvalidInput)?;
    let model = files.read(input, DEFAULT_MAX_BYTES)?;
    let evidence = command
        .evidence
        .as_ref()
        .map(|path| files.read(path, DEFAULT_MAX_BYTES))
        .transpose()?
        .map(|text| serde_json::from_str::<Value>(&text).map_err(|_| CommandError::InvalidInput))
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));
    let request = AnalysisRequest::bim(model)?
        .with_options(AnalysisOptions {
            check_dependencies: !command.without_dependencies,
            extended: command.extended,
        })
        .with_evidence(evidence)?;
    analyze(backend, &request, cancelled).map_err(CommandError::from)
}

/// Render filtered findings while preserving complete per-rule coverage.
///
/// # Errors
/// Output failures propagate; an incomplete write is never reported as complete.
pub fn write_report(
    command: &AnalyzeCommand,
    report: &AnalysisReport,
    output: &mut impl Write,
) -> std::io::Result<u8> {
    let mut displayed = report.clone();
    displayed
        .findings
        .retain(|finding| finding.severity >= command.severity);
    displayed.detail.insert(
        "presentation".into(),
        serde_json::json!({
            "minimumSeverity": command.severity,
            "totalFindings": report.findings.len(),
            "displayedFindings": displayed.findings.len(),
        }),
    );
    serde_json::to_writer(&mut *output, &displayed).map_err(std::io::Error::other)?;
    writeln!(output)?;
    Ok(match report.outcome(command.severity, command.strict) {
        AnalysisOutcome::Complete => 0,
        AnalysisOutcome::Incomplete => 1,
        AnalysisOutcome::Findings => 3,
    })
}
