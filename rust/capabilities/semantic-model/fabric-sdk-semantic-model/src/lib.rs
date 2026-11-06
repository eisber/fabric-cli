// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only BPA contracts, independent of CLI, identity and the managed runtime.
//!
//! The `dotnet` feature selects the separately isolated TOM binding. Default
//! consumers can validate requests and reports or inject their own backend
//! without compiling or initializing .NET.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// Exact upstream Semantic Link Labs implementation used by this rule port.
pub const LABS_COMMIT: &str = "2ddf46109c1209ceffa88149f4631e7b2275f9a1";
/// Number of rules in the pinned Labs source inventory.
pub const RULE_COUNT: usize = 64;
/// Version of the native-to-managed analysis protocol.
pub const SCHEMA_VERSION: &str = "1";
/// Default finite serialized request and response size.
pub const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;

/// One required rule identity derived from the exact pinned name and scopes.
#[derive(Clone, Debug, Deserialize)]
pub struct RuleIdentity {
    /// Stable source-derived identity.
    pub id: String,
    /// Exact upstream rule name.
    pub name: String,
}

/// Return the shipped complete rule identity catalog.
///
/// The developer generator verifies the original source hash and parses inert
/// syntax only. Runtime consumers neither import Python nor consult test files.
pub fn rule_catalog() -> &'static [RuleIdentity] {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Catalog {
        source_commit: String,
        rules: Vec<RuleIdentity>,
    }
    static CATALOG: OnceLock<Vec<RuleIdentity>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let catalog: Catalog = serde_json::from_str(include_str!("rule_catalog.json"))
            .expect("generated BPA catalog is checked before integration");
        assert_eq!(catalog.source_commit, LABS_COMMIT);
        assert_eq!(catalog.rules.len(), RULE_COUNT);
        catalog.rules
    })
}

/// Source model syntax, never a remote endpoint or a script to execute.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFormat {
    /// A TOM database object serialized as model.bim JSON.
    Bim,
    /// A TMDL representation supported by the selected managed loader.
    Tmdl,
}

/// Explicit options controlling evidence-dependent rule families.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisOptions {
    /// Evaluate dependency-dependent rules when dependency evidence is available.
    ///
    /// Disabling collection does not turn unknown dependencies into a known-empty
    /// dependency graph or a clean analysis.
    #[serde(rename = "dependencies")]
    pub check_dependencies: bool,
    /// Evaluate extended-statistics rules only with explicitly supplied evidence.
    pub extended: bool,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            check_dependencies: true,
            extended: false,
        }
    }
}

/// Bounded local model and explicit evidence; Debug never prints model content.
pub struct AnalysisRequest {
    format: ModelFormat,
    model: String,
    options: AnalysisOptions,
    evidence: Value,
    max_bytes: usize,
}

impl std::fmt::Debug for AnalysisRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnalysisRequest")
            .field("format", &self.format)
            .field("model_bytes", &self.model.len())
            .field("options", &self.options)
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

impl AnalysisRequest {
    /// Validate a local BIM database before invoking a backend.
    ///
    /// This checks the JSON envelope, not TOM, DAX or M semantics. The managed
    /// loader remains responsible for native model-version and grammar checks.
    ///
    /// # Errors
    /// Rejects empty/oversized content, executable TMSL envelopes and absent model.
    pub fn bim(model: String) -> Result<Self, AnalysisError> {
        if model.is_empty() || model.len() > DEFAULT_MAX_BYTES {
            return Err(AnalysisError::InvalidInput);
        }
        let value: Value = serde_json::from_str(&model).map_err(|_| AnalysisError::InvalidInput)?;
        let object = value.as_object().ok_or(AnalysisError::InvalidInput)?;
        if !object.get("model").is_some_and(Value::is_object)
            || object.contains_key("createOrReplace")
            || object.contains_key("alter")
            || object.contains_key("refresh")
            || object.contains_key("delete")
        {
            return Err(AnalysisError::InvalidInput);
        }
        Ok(Self {
            format: ModelFormat::Bim,
            model,
            options: AnalysisOptions::default(),
            evidence: json!({}),
            max_bytes: DEFAULT_MAX_BYTES,
        })
    }

    /// Select explicit analysis options; no remote collection is implicit.
    pub fn with_options(mut self, options: AnalysisOptions) -> Self {
        self.options = options;
        self
    }

    /// Supply caller-owned evidence without consulting notebook or identity state.
    ///
    /// # Errors
    /// Evidence must be an object and fit the complete serialized request budget.
    pub fn with_evidence(mut self, evidence: Value) -> Result<Self, AnalysisError> {
        if !evidence.is_object() {
            return Err(AnalysisError::InvalidInput);
        }
        self.evidence = evidence;
        self.to_json()?;
        Ok(self)
    }

    /// Return the serialized, bounded binding request.
    ///
    /// # Errors
    /// Serialization and total byte-limit failures remain explicit.
    pub fn to_json(&self) -> Result<Vec<u8>, AnalysisError> {
        let bytes = serde_json::to_vec(&json!({
            "schemaVersion": SCHEMA_VERSION,
            "format": self.format,
            "model": self.model,
            "options": self.options,
            "evidence": self.evidence,
        }))
        .map_err(|_| AnalysisError::InvalidInput)?;
        if bytes.len() > self.max_bytes {
            return Err(AnalysisError::Limit);
        }
        Ok(bytes)
    }
}

/// Ordered BPA severity, separate from runtime and evidence completeness.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational modeling guidance.
    #[serde(alias = "Info")]
    Info,
    /// Potential modeling or performance concern.
    #[serde(alias = "Warning")]
    Warning,
    /// An error-severity upstream rule finding.
    #[serde(alias = "Error")]
    Error,
}

/// One upstream-shaped rule finding; no automatic fix is implied.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Upstream rule category.
    pub category: String,
    /// Exact upstream rule name.
    pub rule_name: String,
    /// Upstream severity.
    pub severity: Severity,
    /// TOM object scope.
    pub object_type: String,
    /// Upstream-qualified object name.
    pub object_name: String,
    /// Upstream explanation when supplied.
    pub description: Option<String>,
    /// Upstream documentation link when supplied.
    pub url: Option<String>,
}

/// Whether one rule was evaluated with sufficient inputs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleStatus {
    /// All necessary inputs were available and the predicate ran.
    Evaluated,
    /// Required input or supported model functionality was unavailable.
    Unavailable,
    /// Evaluation failed; no clean-result inference is permitted.
    Failed,
}

/// Per-rule evidence, retained even when findings are filtered for presentation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleEvaluation {
    /// Stable source rule identifier.
    pub id: String,
    /// Exact upstream name.
    pub rule_name: String,
    /// Evaluation outcome.
    pub status: RuleStatus,
    /// Additional native diagnostics and provenance, preserved without guessing.
    #[serde(flatten)]
    pub detail: BTreeMap<String, Value>,
}

/// Aggregate rule counts, validated against actual per-rule outcomes.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Coverage {
    /// Number of rules in the pinned inventory.
    pub total: usize,
    /// Rules actually evaluated.
    pub evaluated: usize,
    /// Rules with unavailable inputs.
    pub unavailable: usize,
    /// Rules whose evaluation failed.
    pub failed: usize,
    /// Additional coverage metadata retained from the native evaluator.
    #[serde(flatten)]
    pub detail: BTreeMap<String, Value>,
}

/// Analysis execution status, independent of whether findings exist.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisStatus {
    /// Every declared rule was evaluated.
    Succeeded,
    /// One or more rules could not be evaluated.
    Partial,
    /// One or more rules failed.
    Failed,
}

/// Source identity supplied by the pinned evaluator.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Source {
    /// Exact rule-source commit.
    pub commit: String,
    /// Other source/provenance fields.
    #[serde(flatten)]
    pub detail: BTreeMap<String, Value>,
}

/// Full analysis evidence; a zero-finding partial report is not a clean scan.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisReport {
    /// Protocol schema version.
    pub schema_version: String,
    /// Pinned upstream rule identity.
    pub source: Source,
    /// Evidence completeness and execution status.
    pub status: AnalysisStatus,
    /// Unfiltered upstream rule violations.
    pub findings: Vec<Finding>,
    /// Per-rule evaluation evidence.
    pub rules: Vec<RuleEvaluation>,
    /// Counts corroborated by the rules array.
    pub coverage: Coverage,
    /// Additional model/provenance metadata.
    #[serde(flatten)]
    pub detail: BTreeMap<String, Value>,
}

impl AnalysisReport {
    /// Decode and validate a bounded managed report.
    ///
    /// # Errors
    /// Rejects malformed, version-drifted, inconsistent or false-clean reports.
    pub fn from_json(bytes: &[u8], max_bytes: usize) -> Result<Self, AnalysisError> {
        if bytes.len() > max_bytes {
            return Err(AnalysisError::Limit);
        }
        let report: Self =
            serde_json::from_slice(bytes).map_err(|_| AnalysisError::InvalidResponse)?;
        let mut ids = BTreeSet::new();
        let mut counts = [0usize; 3];
        for rule in &report.rules {
            if !rule_catalog()
                .iter()
                .any(|expected| expected.id == rule.id && expected.name == rule.rule_name)
                || !ids.insert(&rule.id)
            {
                return Err(AnalysisError::InvalidResponse);
            }
            counts[match rule.status {
                RuleStatus::Evaluated => 0,
                RuleStatus::Unavailable => 1,
                RuleStatus::Failed => 2,
            }] += 1;
        }
        let status = if counts[2] != 0 {
            AnalysisStatus::Failed
        } else if counts[1] != 0 {
            AnalysisStatus::Partial
        } else {
            AnalysisStatus::Succeeded
        };
        if report.schema_version != SCHEMA_VERSION
            || report.source.commit != LABS_COMMIT
            || report.rules.len() != RULE_COUNT
            || report.coverage.total != report.rules.len()
            || counts
                != [
                    report.coverage.evaluated,
                    report.coverage.unavailable,
                    report.coverage.failed,
                ]
            || report.status != status
            || report.findings.iter().any(|finding| {
                !report
                    .rules
                    .iter()
                    .any(|rule| rule.rule_name == finding.rule_name)
            })
        {
            return Err(AnalysisError::InvalidResponse);
        }
        Ok(report)
    }

    /// Choose an exit policy without discarding missing-input evidence.
    ///
    /// Incomplete or failed analysis always fails. `strict` additionally fails a
    /// completed scan with findings at or above the selected severity.
    pub fn outcome(&self, minimum: Severity, strict: bool) -> AnalysisOutcome {
        match self.status {
            AnalysisStatus::Partial | AnalysisStatus::Failed => AnalysisOutcome::Incomplete,
            AnalysisStatus::Succeeded
                if strict
                    && self
                        .findings
                        .iter()
                        .any(|finding| finding.severity >= minimum) =>
            {
                AnalysisOutcome::Findings
            }
            AnalysisStatus::Succeeded => AnalysisOutcome::Complete,
        }
    }
}

/// CLI-independent strictness outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalysisOutcome {
    /// Complete analysis under the selected findings policy.
    Complete,
    /// Complete scan with strict-policy findings.
    Findings,
    /// Evaluation failed or lacks necessary evidence.
    Incomplete,
}

/// Explicit native input, runtime or evidence failure, without model contents.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    /// Invalid local model or options.
    #[error("The analysis input must be a supported, bounded local model definition.")]
    InvalidInput,
    /// A finite request/response limit was exceeded.
    #[error("The analysis request or response exceeded its byte limit.")]
    Limit,
    /// The optional managed analyzer was not compiled.
    #[error("Semantic-model BPA requires the optional dotnet feature.")]
    Unsupported,
    /// Runtime activation or managed evaluation failed.
    #[error("The managed analysis backend failed; no clean result is available.")]
    Backend,
    /// A sanitized typed binding diagnostic, never model or provider text.
    #[error("{0}")]
    Binding(String),
    /// The managed protocol/report failed validation.
    #[error("The analysis backend returned invalid or inconsistent evidence.")]
    InvalidResponse,
    /// Cancellation was observed at a safe call boundary.
    #[error("Analysis was cancelled; no complete scan is claimed.")]
    Cancelled,
}

/// Caller-owned backend, without a CLI, process environment or identity edge.
pub trait AnalysisBackend {
    /// Evaluate one serialized request and return the unmodified managed report.
    ///
    /// # Errors
    /// Backend errors cannot be replaced with a successful empty report.
    fn analyze(&self, request: &[u8], max_response_bytes: usize) -> Result<Vec<u8>, AnalysisError>;
}

/// Validate and run one read-only analysis through caller-selected dependencies.
///
/// Cancellation is checked before invocation and after the synchronous backend
/// returns. This API never detaches a running managed call or pretends to abort
/// it. A backend with finer cooperative cancellation must document that contract.
///
/// # Errors
/// Retains input, cancellation, backend and response-evidence failures.
pub fn analyze(
    backend: &impl AnalysisBackend,
    request: &AnalysisRequest,
    cancelled: &AtomicBool,
) -> Result<AnalysisReport, AnalysisError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(AnalysisError::Cancelled);
    }
    let response = backend.analyze(&request.to_json()?, request.max_bytes)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(AnalysisError::Cancelled);
    }
    AnalysisReport::from_json(&response, request.max_bytes)
}

/// Safe caller-configured .NET backend, compiled only through explicit opt-in.
#[cfg(feature = "dotnet")]
pub struct DotnetBackend {
    analyzer: fabric_semantic_model_dotnet::Analyzer,
}

#[cfg(feature = "dotnet")]
impl DotnetBackend {
    /// Load a trusted runtime and managed deployment from explicit code locations.
    ///
    /// # Errors
    /// Missing/incompatible runtime and configuration failures stay explicit.
    pub fn new(
        hostfxr: impl Into<std::path::PathBuf>,
        component_directory: impl Into<std::path::PathBuf>,
    ) -> Result<Self, AnalysisError> {
        fabric_semantic_model_dotnet::Analyzer::new(fabric_semantic_model_dotnet::HostOptions::new(
            hostfxr,
            component_directory,
        ))
        .map(|analyzer| Self { analyzer })
        .map_err(binding_error)
    }

    /// Use Cargo's managed build output for development, not portable deployment.
    ///
    /// # Errors
    /// The caller still supplies an absolute trusted hostfxr library.
    pub fn bundled(hostfxr: impl Into<std::path::PathBuf>) -> Result<Self, AnalysisError> {
        let options =
            fabric_semantic_model_dotnet::HostOptions::bundled(hostfxr).map_err(binding_error)?;
        fabric_semantic_model_dotnet::Analyzer::new(options)
            .map(|analyzer| Self { analyzer })
            .map_err(binding_error)
    }
}

#[cfg(feature = "dotnet")]
impl AnalysisBackend for DotnetBackend {
    fn analyze(&self, request: &[u8], max_response_bytes: usize) -> Result<Vec<u8>, AnalysisError> {
        let response = self
            .analyzer
            .analyze_json(
                request,
                &fabric_semantic_model_dotnet::CancellationToken::new(),
            )
            .map_err(binding_error)?;
        if response.len() > max_response_bytes {
            return Err(AnalysisError::Limit);
        }
        Ok(response)
    }
}

#[cfg(feature = "dotnet")]
fn binding_error(error: fabric_semantic_model_dotnet::BindingError) -> AnalysisError {
    use fabric_semantic_model_dotnet::BindingError;
    match error {
        BindingError::FeatureDisabled | BindingError::UnsupportedFormat => {
            AnalysisError::Unsupported
        }
        BindingError::Cancelled => AnalysisError::Cancelled,
        BindingError::RequestTooLarge | BindingError::ResponseTooLarge => AnalysisError::Limit,
        BindingError::InvalidRequest => AnalysisError::InvalidInput,
        BindingError::InvalidResponse | BindingError::UnsupportedSchema => {
            AnalysisError::InvalidResponse
        }
        error => AnalysisError::Binding(error.to_string()),
    }
}
