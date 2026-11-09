// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{RunError, RunTimeout};
use crate::JobFuture;
use serde_json::Value;
use std::fmt;

/// Exact artifact-owner discriminant, independent of CLI aliases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOwnerKind {
    /// Whole Notebook.
    Notebook,
    /// Published UserDataFunction item.
    UserDataFunction,
    /// Lakehouse owning an explicitly selected Livy session.
    Lakehouse,
}

/// Validated route owner; construction performs no remote type lookup.
#[derive(Clone, Eq, PartialEq)]
pub struct RunOwner {
    kind: RunOwnerKind,
    workspace: String,
    item: String,
}

impl RunOwner {
    /// Validate route-safe workspace/item identifiers before credential access.
    pub fn new(kind: RunOwnerKind, workspace: &str, item: &str) -> Result<Self, RunError> {
        for id in [workspace, item] {
            crate::identifier(id).map_err(|_| RunError::InvalidTarget)?;
        }
        Ok(Self {
            kind,
            workspace: workspace.into(),
            item: item.into(),
        })
    }
    /// Owner artifact type.
    pub const fn kind(&self) -> RunOwnerKind {
        self.kind
    }
    /// Exact workspace identifier.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    /// Exact item identifier.
    pub fn item(&self) -> &str {
        &self.item
    }
}

impl fmt::Debug for RunOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunOwner")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Typed execution target, including explicitly unsupported future targets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunTargetKind {
    /// Native whole-Notebook execution.
    Notebook,
    /// Synchronous exact published function.
    Udf,
    /// Statement in an existing explicitly selected session.
    LivyStatement,
    /// Unsupported saved-cell target, never coerced into whole-Notebook run.
    NotebookCell,
    /// Deferred owned one-shot; default budget is defined but capability is not.
    LivyOneShot,
}

/// One validated owner/facet binding; no implicit session or function selection.
#[derive(Clone, Eq, PartialEq)]
pub struct RunTarget {
    owner: RunOwner,
    kind: RunTargetKind,
    function: Option<String>,
    session: Option<u64>,
}

impl RunTarget {
    /// Whole item, cell or deferred one-shot shape; rejects wrong owner locally.
    pub fn item(owner: RunOwner, kind: RunTargetKind) -> Result<Self, RunError> {
        let expected = match kind {
            RunTargetKind::Notebook | RunTargetKind::NotebookCell => RunOwnerKind::Notebook,
            RunTargetKind::LivyOneShot => RunOwnerKind::Lakehouse,
            _ => return Err(RunError::InvalidTarget),
        };
        Self::checked(owner, kind, expected, None, None)
    }
    /// Exact function name with no fabricated invocation monitor.
    pub fn udf(owner: RunOwner, function: &str) -> Result<Self, RunError> {
        crate::identifier(function).map_err(|_| RunError::InvalidTarget)?;
        Self::checked(
            owner,
            RunTargetKind::Udf,
            RunOwnerKind::UserDataFunction,
            Some(function.into()),
            None,
        )
    }
    /// Explicit borrowed-session statement target; session zero is valid.
    pub fn livy(owner: RunOwner, session: u64) -> Result<Self, RunError> {
        Self::checked(
            owner,
            RunTargetKind::LivyStatement,
            RunOwnerKind::Lakehouse,
            None,
            Some(session),
        )
    }
    fn checked(
        owner: RunOwner,
        kind: RunTargetKind,
        expected: RunOwnerKind,
        function: Option<String>,
        session: Option<u64>,
    ) -> Result<Self, RunError> {
        if owner.kind != expected {
            return Err(RunError::WrongOwner);
        }
        Ok(Self {
            owner,
            kind,
            function,
            session,
        })
    }
    /// Exact validated artifact owner.
    pub fn owner(&self) -> &RunOwner {
        &self.owner
    }
    /// Selected target discriminant.
    pub const fn kind(&self) -> RunTargetKind {
        self.kind
    }
    /// Published function, present only on a UDF target.
    pub fn function(&self) -> Option<&str> {
        self.function.as_deref()
    }
    /// Borrowed Livy session, never missing because its numeric value is zero.
    pub const fn session(&self) -> Option<u64> {
        self.session
    }
}

impl fmt::Debug for RunTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunTarget")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Input omission remains distinct from JSON null and an empty object.
#[derive(Clone)]
pub enum RunInput {
    /// No input supplied; legal only when the selected binding advertises it.
    Omitted,
    /// Exact native JSON value; no defaults are inserted by the foundation.
    Json(Value),
}

impl fmt::Debug for RunInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Omitted => "Omitted",
            Self::Json(_) => "Json(<redacted>)",
        })
    }
}

/// Native payload shape; adapters validate their richer schema in preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunInputRule {
    /// Omission or native object, never null/array/scalar.
    OptionalObject,
    /// A native object must be explicitly supplied.
    RequiredObject,
}

/// Supported result representation; no implicit conversion of native bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunFormat {
    /// Structured JSON evidence/native result.
    Json,
    /// Binding-advertised textual result.
    Text,
    /// Binding-advertised native bytes.
    Binary,
}

/// Adapter-declared supported target/schema/representation contract.
#[derive(Clone, Debug)]
pub struct RunCapabilities {
    /// Must equal the selected target, not just its artifact owner.
    pub target: RunTargetKind,
    /// Exact omission/native-object rule.
    pub input: RunInputRule,
    /// Whether waiting is supported; synchronous UDF accepts redundant wait.
    pub wait: bool,
    /// Formats supported by this binding.
    pub formats: Vec<RunFormat>,
}

/// Host-neutral options after CLI/SDK/MCP normalization.
#[derive(Clone, Copy, Debug)]
pub struct RunOptions<'a> {
    /// Observe the newly submitted run; redundant for a synchronous UDF.
    pub wait: bool,
    /// Explicit duration overrides the selected mode's default.
    pub timeout: Option<&'a str>,
    /// Selected binding-supported representation.
    pub format: RunFormat,
}

/// Caller-owned no-clobber output, never a source of execution replay.
pub trait RunOutput: Send + Sync {
    /// Pure local destination check, before deadline/credentials/HTTP.
    fn preflight(&self, format: RunFormat) -> Result<(), RunError>;
    /// Atomic no-clobber commit; a final collision/error must return Output.
    fn commit<'a>(&'a self, bytes: &'a [u8]) -> JobFuture<'a, Result<(), RunError>>;
}

/// Validated plan, produced before any effect or monotonic budget starts.
#[derive(Clone, Debug)]
pub struct RunPreflight {
    target: RunTarget,
    input: RunInput,
    timeout: RunTimeout,
    wait: bool,
    format: RunFormat,
}

impl RunPreflight {
    /// Validate locally detectable target/schema/format/output errors.
    ///
    /// `validate_native` is the selected adapter's pure schema validator; it
    /// must not fetch schemas, acquire credentials or execute stored code.
    pub fn validate(
        target: RunTarget,
        capabilities: &RunCapabilities,
        options: RunOptions<'_>,
        input: RunInput,
        output: &dyn RunOutput,
        validate_native: impl FnOnce(&RunTarget, &RunInput) -> Result<(), RunError>,
    ) -> Result<Self, RunError> {
        if matches!(
            target.kind,
            RunTargetKind::NotebookCell | RunTargetKind::LivyOneShot
        ) || target.kind != capabilities.target
            || (options.wait && !capabilities.wait)
            || !capabilities.formats.contains(&options.format)
        {
            return Err(RunError::UnsupportedCapability);
        }
        match (&input, capabilities.input) {
            (RunInput::Omitted, RunInputRule::OptionalObject) => (),
            (RunInput::Json(value), _) if value.is_object() => (),
            _ => return Err(RunError::InvalidInput),
        }
        let timeout = options.timeout.map_or_else(
            || Ok(RunTimeout::for_target(target.kind, options.wait)),
            RunTimeout::parse,
        )?;
        validate_native(&target, &input)?;
        output.preflight(options.format)?;
        Ok(Self {
            target,
            input,
            timeout,
            wait: options.wait,
            format: options.format,
        })
    }
    /// Original exact target, shared by all consumer heads.
    pub fn target(&self) -> &RunTarget {
        &self.target
    }
    /// Original omission/null/empty-preserving input.
    pub fn input(&self) -> &RunInput {
        &self.input
    }
    /// Validated total budget.
    pub const fn timeout(&self) -> RunTimeout {
        self.timeout
    }
    /// Requested wait spelling, not an invented UDF asynchronous mode.
    pub const fn wait(&self) -> bool {
        self.wait
    }
    /// Requested native result representation.
    pub const fn format(&self) -> RunFormat {
        self.format
    }
}
