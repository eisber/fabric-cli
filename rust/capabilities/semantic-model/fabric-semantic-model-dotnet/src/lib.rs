// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional in-process .NET/TOM hosting with a safe, bounded JSON interface.
//!
//! The empty default feature set neither compiles managed code nor loads .NET.
//! Opting in requires explicit trusted hostfxr and component paths. Analysis is
//! synchronous, offline and read-only; callers own scheduling and cancellation.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

#[cfg(feature = "dotnet")]
mod interop;

/// Maximum UTF-8 transport request size, including the serialized model.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum UTF-8 analyzer response size.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Current request protocol version.
pub const SCHEMA_VERSION: &str = "1";

/// Sanitized transport errors; these never contain model text or evidence.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum BindingError {
    /// Analysis was requested without the opt-in Cargo feature.
    #[error("Semantic-model analysis requires the optional dotnet feature.")]
    FeatureDisabled,
    /// The caller cancelled before execution or before result delivery.
    #[error("Semantic-model analysis was cancelled.")]
    Cancelled,
    /// The JSON request exceeds the hard byte limit.
    #[error("Semantic-model request exceeds the 16 MiB limit.")]
    RequestTooLarge,
    /// The JSON request is malformed or has invalid fields.
    #[error("Invalid semantic-model analysis request.")]
    InvalidRequest,
    /// Only protocol schema version 1 is supported.
    #[error("Unsupported semantic-model analysis schema version.")]
    UnsupportedSchema,
    /// This boundary does not yet support faithful TMDL loading.
    #[error("Unsupported semantic-model format; this boundary currently supports BIM only.")]
    UnsupportedFormat,
    /// Explicit runtime/component configuration was invalid.
    #[error(
        "Invalid .NET host configuration; provide absolute trusted hostfxr and component paths."
    )]
    InvalidConfiguration,
    /// A runtime or required managed component could not be loaded.
    #[error("The .NET 8 runtime or managed TOM component is unavailable or incompatible.")]
    RuntimeUnavailable,
    /// A process has already loaded a different managed component/runtime pair.
    #[error("The process already hosts a different semantic-model runtime configuration.")]
    RuntimeAlreadyConfigured,
    /// TOM could not load a valid offline database.
    #[error("The BIM model could not be loaded as a TOM database.")]
    MalformedModel,
    /// A managed exception was contained by the boundary.
    #[error("Managed semantic-model analysis failed.")]
    ManagedFailure,
    /// Managed output exceeded the hard response byte limit.
    #[error("Semantic-model response exceeds the 16 MiB limit.")]
    ResponseTooLarge,
    /// Managed output or the native ABI contract was invalid.
    #[error("The managed analyzer returned an invalid response.")]
    InvalidResponse,
    /// A hosting lock was poisoned by a prior Rust panic.
    #[error("The semantic-model host is no longer available after a host failure.")]
    HostUnavailable,
}

/// Cloneable cooperative cancellation state; cancellation never unloads .NET.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Creates an uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation, including suppression of an in-flight result.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    #[cfg(feature = "dotnet")]
    pub(crate) fn check(&self) -> Result<(), BindingError> {
        if self.is_cancelled() {
            Err(BindingError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Explicit trusted code locations; no PATH, credential or settings discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostOptions {
    hostfxr_path: PathBuf,
    component_directory: PathBuf,
}

impl HostOptions {
    /// Selects an absolute hostfxr library and managed deployment directory.
    ///
    /// These are executable-code trust inputs, not paths taken from model data.
    /// The directory must contain the DLL, deps/runtimeconfig JSON and TOM DLLs.
    #[must_use]
    pub fn new(hostfxr_path: impl Into<PathBuf>, component_directory: impl Into<PathBuf>) -> Self {
        Self {
            hostfxr_path: hostfxr_path.into(),
            component_directory: component_directory.into(),
        }
    }

    /// Selects Cargo's feature-on managed build output and an explicit hostfxr.
    ///
    /// For distribution, copy the complete output and use [`Self::new`] instead
    /// of retaining a path to the build machine's Cargo output directory.
    pub fn bundled(hostfxr_path: impl Into<PathBuf>) -> Result<Self, BindingError> {
        #[cfg(feature = "dotnet")]
        {
            Ok(Self::new(
                hostfxr_path,
                PathBuf::from(env!("FABRIC_BPA_COMPONENT_DIR")),
            ))
        }
        #[cfg(not(feature = "dotnet"))]
        {
            let _ = hostfxr_path;
            Err(BindingError::FeatureDisabled)
        }
    }

    /// Returns the explicitly configured hostfxr library path.
    #[must_use]
    pub fn hostfxr_path(&self) -> &Path {
        &self.hostfxr_path
    }

    /// Returns the explicitly configured component directory.
    #[must_use]
    pub fn component_directory(&self) -> &Path {
        &self.component_directory
    }
}

/// Reusable safe handle to the process-lifetime in-process managed analyzer.
#[derive(Clone)]
pub struct Analyzer {
    #[cfg(feature = "dotnet")]
    runtime: Arc<interop::Runtime>,
}

impl std::fmt::Debug for Analyzer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Analyzer").finish_non_exhaustive()
    }
}

impl Analyzer {
    /// Loads the configured .NET runtime/component without model or live I/O.
    ///
    /// The first successful configuration is retained for process lifetime.
    /// Later handles must use the same canonical code locations.
    pub fn new(options: HostOptions) -> Result<Self, BindingError> {
        #[cfg(feature = "dotnet")]
        {
            Ok(Self {
                runtime: interop::Runtime::load(options)?,
            })
        }
        #[cfg(not(feature = "dotnet"))]
        {
            let _ = options;
            Err(BindingError::FeatureDisabled)
        }
    }

    /// Analyzes a bounded UTF-8 JSON request and returns validated JSON bytes.
    ///
    /// Calls serialize across handles. Cancellation is checked before waiting,
    /// after acquiring the execution gate, and after managed execution. TOM and
    /// the evaluator cannot safely be preempted: in-flight cancellation discards
    /// the result after evaluation finishes and does not roll back or kill code.
    /// An async consumer must schedule this synchronous CPU work appropriately.
    pub fn analyze_json(
        &self,
        request: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, BindingError> {
        #[cfg(feature = "dotnet")]
        {
            cancellation.check()?;
            validate_request_json(request)?;
            self.runtime.analyze(request, cancellation)
        }
        #[cfg(not(feature = "dotnet"))]
        {
            let _ = (request, cancellation);
            Err(BindingError::FeatureDisabled)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    schema_version: String,
    format: String,
    model: String,
    options: Value,
    evidence: Value,
}

/// Validates transport shape and limits without loading a runtime.
///
/// Exactly the five documented fields are required; duplicate/unknown fields,
/// non-object options/evidence and JSON nesting beyond serde's bound fail.
/// BIM database contents are validated separately by real TOM deserialization.
pub fn validate_request_json(request: &[u8]) -> Result<(), BindingError> {
    if request.len() > MAX_REQUEST_BYTES {
        return Err(BindingError::RequestTooLarge);
    }
    let parsed: Request =
        serde_json::from_slice(request).map_err(|_| BindingError::InvalidRequest)?;
    if parsed.schema_version != SCHEMA_VERSION {
        return Err(BindingError::UnsupportedSchema);
    }
    if parsed.format != "bim" {
        return Err(BindingError::UnsupportedFormat);
    }
    if parsed.model.is_empty() || !parsed.options.is_object() || !parsed.evidence.is_object() {
        return Err(BindingError::InvalidRequest);
    }
    Ok(())
}

#[cfg(feature = "dotnet")]
fn validate_response_json(response: &[u8]) -> Result<(), BindingError> {
    let parsed: Value =
        serde_json::from_slice(response).map_err(|_| BindingError::InvalidResponse)?;
    if parsed.get("schemaVersion").and_then(Value::as_str) != Some(SCHEMA_VERSION)
        || !parsed.get("source").is_some_and(Value::is_object)
        || !parsed.get("status").is_some_and(Value::is_string)
        || !parsed.get("findings").is_some_and(Value::is_array)
        || !parsed.get("rules").is_some_and(Value::is_array)
        || !parsed.get("coverage").is_some_and(Value::is_object)
    {
        return Err(BindingError::InvalidResponse);
    }
    Ok(())
}
