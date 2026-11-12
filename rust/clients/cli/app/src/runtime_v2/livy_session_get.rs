// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact classic Livy session GET leaf over the shared runtime read head.
//!
//! The caller owns binding, authorization, deadlines and output. This module
//! neither registers executable routes nor selects a service profile.

use super::{
    OutputSink, OwnerKind, ParsedInvocation, ReadError, ReadErrorKind, ReadFacet, ReadOperation,
    ReadReceipt, RuntimeHead,
};

/// Whether an already parsed invocation selects this leaf, without resolving it.
///
/// Collections, statements, Notebook sessions and other verbs remain unclaimed.
/// This is route selection only; [`run`] retains the head's independent input,
/// owner, SDK ID, output and response validation.
pub fn claims(invocation: &ParsedInvocation) -> bool {
    invocation.operation == ReadOperation::Get
        && matches!(
            invocation.path.owner.kind,
            OwnerKind::Lakehouse | OwnerKind::LivyService
        )
        && matches!(invocation.path.facet, ReadFacet::Session { .. })
}

/// Read exactly the selected classic session through the existing head and SDK.
///
/// No profile discovery, list/latest lookup, session creation or output replay is
/// performed. Read completion remains separate from workload readiness/success.
///
/// # Errors
/// Rejects foreign invocations before resolver, executor or sink access, and
/// otherwise preserves the shared head's validation, native and delivery errors.
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
