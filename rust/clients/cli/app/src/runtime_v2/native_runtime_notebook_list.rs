// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Notebook Jupyter collection leaf over the existing runtime read engine.
//!
//! The executable host owns authorization, immutable binding, deadlines and output.
//! This leaf neither discovers those dependencies nor claims foreign runtime routes.

use super::{
    OutputSink, OwnerKind, ParsedInvocation, ReadError, ReadErrorKind, ReadFacet, ReadOperation,
    ReadReceipt, RuntimeHead,
};

/// Whether this leaf owns the normalized Notebook session collection invocation.
///
/// Get, content, exact-session, Lakehouse and Livy routes remain with their owners.
/// The shared parser and head still independently validate the complete input.
pub fn claims(invocation: &ParsedInvocation) -> bool {
    invocation.operation == ReadOperation::List
        && invocation.path.owner.kind == OwnerKind::Notebook
        && invocation.path.facet == ReadFacet::Sessions
}

/// Read one whole bounded live Jupyter collection through the shared runtime head.
///
/// Both canonical and Notebook facade spellings use the same normalized input,
/// SDK method, receipt and output attempt. `--limit` is a whole-collection bound,
/// not a Jupyter paging query. There is no readiness inference, history fallback,
/// active/newest selection, retry or session lifecycle operation.
///
/// # Errors
/// Rejects foreign invocations before resolution/output/HTTP. Otherwise preserves
/// the shared head's input, binding, native, budget and delivery failures.
pub async fn run(
    head: &RuntimeHead<'_>,
    invocation: &ParsedInvocation,
    output: &mut dyn OutputSink,
) -> Result<ReadReceipt, ReadError> {
    if !claims(invocation) {
        return Err(ReadError::new(ReadErrorKind::UnsupportedCapability));
    }
    head.run(invocation, output).await
}

/// Acquire a collection using the released native host and its original deadline.
///
/// The caller must preflight the destination, supply the already authorized
/// executor/resolver, and pin one cancellation future across preflight,
/// acquisition and delivery. The returned observation is delivered at most once
/// through [`super::host::PendingRead::deliver`].
///
/// # Errors
/// Rejects foreign invocations before shared-host access, then preserves the
/// shared host's validation, native, budget, cancellation and deadline failures.
#[cfg(feature = "native-host")]
pub async fn acquire(
    invocation: &ParsedInvocation,
    executor: &dyn fabric_sdk::runtime::NativeExecutor,
    resolver: &dyn super::RuntimeResolver,
    options: super::ReadOptions,
    destination: &super::host::Destination,
    deadline: std::time::Instant,
    cancelled: impl std::future::Future<Output = ()>,
) -> Result<super::host::PendingRead, super::host::HostError> {
    if !claims(invocation) {
        return Err(ReadError::new(ReadErrorKind::UnsupportedCapability).into());
    }
    super::host::acquire(
        invocation,
        executor,
        resolver,
        options,
        destination,
        deadline,
        cancelled,
    )
    .await
}
