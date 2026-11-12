// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Experimental H1 common/facade reads over the composed native SDK.
//!
//! This library head needs caller-owned resolution, authenticated native execution,
//! deadlines and an explicit output sink. It does not wire the v1 executable.
//! See `rust/authoring-docs/runtime-v2-reads.md` for grammar, supported options and host gates.

#![deny(missing_docs)]

mod contract;
#[cfg(feature = "native-host")]
pub mod host;
#[cfg(feature = "notebook-workload")]
pub mod notebook;
mod parser;
mod path;

pub use contract::*;
pub use parser::{OPERATIONS, OperationRegistration, command_schema, parse_args};
pub use path::{OwnerKind, OwnerPath, ReadFacet, TypedPath};

use fabric_sdk::{
    livy::{
        ActivityList, ActivityPage, BytePage, LivyRequest, LivyService, Observation, ResponseModel,
        ServiceProfile, SessionSnapshot, StatementSnapshot,
    },
    notebook_runtime::{NotebookRuntime, SessionId},
    runtime::{
        NativeEffect, NativeError, NativeExecutor, NativeMethod, NativeRequest, NativeResponse,
    },
};
use serde::Serialize;
use serde_json::Value;
use std::{
    future::Future,
    io::{self, Write},
    pin::Pin,
    sync::atomic::{AtomicU16, AtomicUsize, Ordering},
};

/// Authoritative caller-owned SDK binding, not a raw URL from command-line input.
pub enum ResolvedTarget {
    /// Native Notebook workload scope and independent SDK content/session budgets.
    Notebook(NotebookRuntime),
    /// Lakehouse-owned Livy binding; the caller explicitly selects the SDK profile.
    Lakehouse(LivyService),
    /// Explicit native Livy service, never selected by fallback from Notebook.
    LivyService(LivyService),
}

impl ResolvedTarget {
    fn kind(&self) -> OwnerKind {
        match self {
            Self::Notebook(_) => OwnerKind::Notebook,
            Self::Lakehouse(_) => OwnerKind::Lakehouse,
            Self::LivyService(_) => OwnerKind::LivyService,
        }
    }
}

/// Resolve explicit human names to authoritative SDK scopes.
///
/// Implementations must use caller-supplied bindings/snapshots. This synchronous
/// seam does not authorize ambient metadata, credential or endpoint discovery.
pub trait RuntimeResolver {
    /// Resolve exactly this owner without guessing active sessions or changing type.
    ///
    /// # Errors
    /// Returns explicit invalid/missing/unsupported binding errors before native execution.
    fn resolve(&self, path: &TypedPath) -> Result<ResolvedTarget, NativeError>;
}

/// Borrowed parser-to-SDK read consumer; no runtime, transport or credential backend.
pub struct RuntimeHead<'a> {
    executor: &'a dyn NativeExecutor,
    resolver: &'a dyn RuntimeResolver,
    options: ReadOptions,
}

impl<'a> RuntimeHead<'a> {
    /// Inject an already authorized native executor, explicit resolver and finite ceilings.
    ///
    /// # Errors
    /// Rejects zero ceilings without resolver/output/executor access.
    pub fn inject(
        executor: &'a dyn NativeExecutor,
        resolver: &'a dyn RuntimeResolver,
        options: ReadOptions,
    ) -> Result<Self, ReadError> {
        options.validate()?;
        Ok(Self {
            executor,
            resolver,
            options,
        })
    }

    /// Execute one parsed read and deliver its bounded receipt once.
    ///
    /// All input/capability/owner checks and output preflight precede dispatch.
    /// HTTP 202 is accepted only. Failed workloads remain successful *reads*.
    ///
    /// # Errors
    /// Native, bound and output failures are explicit. A delivery error retains
    /// the remote receipt; the head never retries HTTP or replays output.
    pub async fn run(
        &self,
        invocation: &ParsedInvocation,
        output: &mut dyn OutputSink,
    ) -> Result<ReadReceipt, ReadError> {
        invocation.validate(self.options)?;
        let target = self.resolver.resolve(&invocation.path)?;
        if target.kind() != invocation.path.owner.kind {
            return Err(ReadError::new(ReadErrorKind::WrongOwner));
        }
        let prepared = PreparedRead::new(target, invocation)?;
        let native_url = prepared.request()?.url().as_str().to_owned();
        output
            .check(&invocation.output)
            .map_err(|_| ReadError::new(ReadErrorKind::OutputUnavailable))?;
        let executor = BoundedExecutor {
            inner: self.executor,
            max_bytes: self.options.max_response_bytes,
            calls: AtomicUsize::new(0),
            status: AtomicU16::new(0),
            bytes: AtomicUsize::new(0),
        };
        let observed = prepared.observe(&executor).await.map_err(|mut error| {
            error.http_status = error.http_status.or(executor.status());
            error
        })?;
        if executor.calls.load(Ordering::Relaxed) != 1 {
            return Err(ReadError::new(ReadErrorKind::InvalidResponse));
        }
        if invocation.limit.is_some_and(|limit| {
            observed
                .data
                .as_array()
                .is_some_and(|rows| rows.len() > limit as usize)
        }) {
            return Err(ReadError::new(ReadErrorKind::BudgetExceeded));
        }
        let mut count = CountingWriter {
            remaining: invocation.max_bytes,
        };
        serde_json::to_writer(&mut count, &observed.data)
            .map_err(|_| ReadError::new(ReadErrorKind::BudgetExceeded))?;
        let status = executor
            .status()
            .ok_or_else(|| ReadError::new(ReadErrorKind::InvalidResponse))?;
        let accepted = status == 202;
        let receipt = ReadReceipt {
            schema_version: "0.1".into(),
            operation_id: invocation.operation,
            target: ReadTarget {
                path: invocation.path.clone(),
                native_url,
            },
            effect: ReadEffect::Read,
            status: if accepted {
                ReadStatus::Accepted
            } else {
                ReadStatus::ReadComplete
            },
            data: observed.data,
            meta: ReadMeta {
                representation: observed.representation,
                http_status: status,
                source_revision: observed.source_revision.into(),
                maturity: "source_modeled_deployment_unverified".into(),
                coverage: if accepted {
                    ReadCoverage::Accepted
                } else {
                    observed.coverage
                },
                continuation: None,
                provider_total: observed.total,
                workload_state: observed.state,
                workload_observation: observed.workload,
                response_bytes: executor.bytes.load(Ordering::Relaxed),
            },
        };
        let bytes = encode_bounded(&receipt, self.options.max_receipt_bytes)?;
        if output.write(&invocation.output, &bytes).is_err() {
            let mut error = ReadError::new(ReadErrorKind::Delivery);
            error.http_status = Some(status);
            error.receipt = Some(Box::new(receipt));
            return Err(error);
        }
        Ok(receipt)
    }
}

enum PreparedRead {
    Content(NotebookRuntime),
    NotebookList(NotebookRuntime),
    NotebookSession(NotebookRuntime, SessionId),
    LivyList(LivyRequest<ActivityList>),
    LivySession(LivyRequest<SessionSnapshot>, String),
    Statement(LivyRequest<StatementSnapshot>, u64),
}

impl PreparedRead {
    fn new(target: ResolvedTarget, input: &ParsedInvocation) -> Result<Self, ReadError> {
        Ok(match target {
            ResolvedTarget::Notebook(runtime) => match &input.path.facet {
                ReadFacet::NotebookContent => Self::Content(runtime),
                ReadFacet::Sessions => Self::NotebookList(runtime),
                ReadFacet::Session { id } => Self::NotebookSession(runtime, SessionId::new(id)?),
                _ => return Err(ReadError::new(ReadErrorKind::UnsupportedCapability)),
            },
            ResolvedTarget::Lakehouse(service) | ResolvedTarget::LivyService(service) => {
                if input.path.owner.kind == OwnerKind::LivyService
                    && service.profile() != ServiceProfile::NativeWildfire
                {
                    return Err(ReadError::new(ReadErrorKind::WrongOwner));
                }
                match &input.path.facet {
                    ReadFacet::Sessions => {
                        let limit = input
                            .limit
                            .ok_or_else(|| ReadError::new(ReadErrorKind::InvalidInput))?;
                        let page = match service.profile() {
                            ServiceProfile::NativeWildfire => ActivityPage::Native {
                                offset: None,
                                size: Some(limit),
                            },
                            ServiceProfile::FabricJson => ActivityPage::Fabric {
                                top: Some(limit),
                                skip: None,
                                count: None,
                            },
                        };
                        Self::LivyList(service.list_sessions(page)?)
                    }
                    ReadFacet::Session { id } => {
                        Self::LivySession(service.session(id)?.get()?, id.clone())
                    }
                    ReadFacet::Statement { session, id, .. } => Self::Statement(
                        service
                            .session(session)?
                            .statements()
                            .statement(*id)?
                            .get(BytePage::default())?,
                        *id,
                    ),
                    _ => return Err(ReadError::new(ReadErrorKind::UnsupportedCapability)),
                }
            }
        })
    }

    fn request(&self) -> Result<NativeRequest, NativeError> {
        match self {
            Self::Content(runtime) => runtime.content_get_request(),
            Self::NotebookList(runtime) => runtime.sessions_request(false),
            Self::NotebookSession(runtime, id) => runtime.session_get_request(id, false),
            Self::LivyList(request) => Ok(request.native().clone()),
            Self::LivySession(request, _) => Ok(request.native().clone()),
            Self::Statement(request, _) => Ok(request.native().clone()),
        }
    }

    async fn observe(self, executor: &BoundedExecutor<'_>) -> Result<Observed, ReadError> {
        let notebook_revision = fabric_sdk::notebook_runtime::SOURCE_REVISION;
        match self {
            Self::Content(runtime) => {
                let document = runtime.get_content(executor).await?;
                Ok(Observed::new(
                    document.as_value().clone(),
                    Representation::NotebookSource,
                    notebook_revision,
                    ReadCoverage::Resource,
                ))
            }
            Self::NotebookList(runtime) => {
                let sessions = runtime.list_sessions(executor, false).await?;
                let rows = sessions
                    .into_iter()
                    .map(|session| session.as_value().clone())
                    .collect();
                Ok(Observed::new(
                    Value::Array(rows),
                    Representation::NotebookSession,
                    notebook_revision,
                    ReadCoverage::ReturnedCollection,
                ))
            }
            Self::NotebookSession(runtime, id) => {
                let session = runtime.get_session(executor, &id, false).await?;
                let value = session.as_value().clone();
                let mut observed = Observed::new(
                    value,
                    Representation::NotebookSession,
                    notebook_revision,
                    ReadCoverage::Resource,
                );
                observed.state = observed
                    .data
                    .get("state")
                    .or_else(|| {
                        observed
                            .data
                            .get("kernel")
                            .and_then(|kernel| kernel.get("execution_state"))
                    })
                    .cloned();
                observed.workload = Some(WorkloadObservation::Unknown);
                Ok(observed)
            }
            Self::LivyList(request) => {
                let response = execute_livy(&request, executor).await?;
                let mut observed = Observed::new(
                    Value::Null,
                    Representation::LivySession,
                    LIVY_REVISION,
                    ReadCoverage::ProviderPage,
                );
                if let Some(model) = response.model() {
                    let sessions = model.sessions()?;
                    observed.data = Value::Array(
                        sessions
                            .iter()
                            .map(|session| session.raw().clone())
                            .collect(),
                    );
                    observed.total = model.total();
                }
                Ok(observed)
            }
            Self::LivySession(request, id) => {
                let response = execute_livy(&request, executor).await?;
                let mut observed = Observed::new(
                    Value::Null,
                    Representation::LivySession,
                    LIVY_REVISION,
                    ReadCoverage::Resource,
                );
                if let Some(model) = response.model() {
                    validate_id(model.raw(), &id)?;
                    observed.data = model.raw().clone();
                    observed.state = model.raw().get("state").cloned();
                    observed.workload = Some(classify(model.observe()));
                }
                Ok(observed)
            }
            Self::Statement(request, id) => {
                let response = execute_livy(&request, executor).await?;
                let mut observed = Observed::new(
                    Value::Null,
                    Representation::LivyStatement,
                    LIVY_REVISION,
                    ReadCoverage::Resource,
                );
                if let Some(model) = response.model() {
                    if model.raw().get("id").and_then(Value::as_u64) != Some(id) {
                        return Err(ReadError::new(ReadErrorKind::InvalidResponse));
                    }
                    observed.data = model.raw().clone();
                    observed.state = model.raw().get("state").cloned();
                    observed.workload = Some(classify(model.observe()));
                }
                Ok(observed)
            }
        }
    }
}

const LIVY_REVISION: &str = fabric_sdk::livy::SOURCE_REVISION;

async fn execute_livy<T: ResponseModel>(
    request: &LivyRequest<T>,
    executor: &BoundedExecutor<'_>,
) -> Result<fabric_sdk::livy::LivyResponse<T>, ReadError> {
    request
        .execute(executor, executor.max_bytes)
        .await
        .map_err(|error| {
            let mut result = ReadError::from(error.native_error());
            result.http_status = error.status();
            result
        })
}

fn validate_id(value: &Value, id: &str) -> Result<(), ReadError> {
    let matches = match value.get("id") {
        Some(Value::String(actual)) => actual == id,
        Some(Value::Number(actual)) => actual.to_string() == id,
        _ => false,
    };
    if !matches {
        return Err(ReadError::new(ReadErrorKind::InvalidResponse));
    }
    Ok(())
}

fn classify(
    observation: Result<Observation, fabric_sdk::livy::WorkloadFailure>,
) -> WorkloadObservation {
    match observation {
        Ok(Observation::Pending) => WorkloadObservation::Pending,
        Ok(Observation::Ready) => WorkloadObservation::Ready,
        Ok(Observation::Running) => WorkloadObservation::Running,
        Ok(Observation::Succeeded) => WorkloadObservation::Succeeded,
        Ok(Observation::Cancelled) => WorkloadObservation::Cancelled,
        Ok(Observation::Unknown) => WorkloadObservation::Unknown,
        Err(_) => WorkloadObservation::Failed,
    }
}

struct Observed {
    data: Value,
    representation: Representation,
    source_revision: &'static str,
    coverage: ReadCoverage,
    state: Option<Value>,
    workload: Option<WorkloadObservation>,
    total: Option<u64>,
}

impl Observed {
    fn new(
        data: Value,
        representation: Representation,
        source_revision: &'static str,
        coverage: ReadCoverage,
    ) -> Self {
        Self {
            data,
            representation,
            source_revision,
            coverage,
            state: None,
            workload: None,
            total: None,
        }
    }
}

struct BoundedExecutor<'a> {
    inner: &'a dyn NativeExecutor,
    max_bytes: usize,
    calls: AtomicUsize,
    status: AtomicU16,
    bytes: AtomicUsize,
}

impl BoundedExecutor<'_> {
    fn status(&self) -> Option<u16> {
        let status = self.status.load(Ordering::Relaxed);
        (status != 0).then_some(status)
    }
}

impl NativeExecutor for BoundedExecutor<'_> {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>> {
        Box::pin(async move {
            if request.method() != NativeMethod::Get
                || request.effect() != NativeEffect::Read
                || self.calls.fetch_add(1, Ordering::Relaxed) != 0
            {
                return Err(NativeError::UnsupportedCapability);
            }
            let limit = self.max_bytes.min(max_response_bytes);
            let response = self.inner.execute(request, limit).await?;
            if response.body.len() > limit {
                return Err(NativeError::Limit);
            }
            self.status.store(response.status, Ordering::Relaxed);
            self.bytes.store(response.body.len(), Ordering::Relaxed);
            Ok(response)
        })
    }
}

struct CountingWriter {
    remaining: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("data bound exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_bounded(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, ReadError> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("receipt bound exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut buffer, value)
        .and_then(|()| buffer.write_all(b"\n").map_err(serde_json::Error::io))
        .map_err(|_| ReadError::new(ReadErrorKind::BudgetExceeded))?;
    Ok(buffer.bytes)
}
