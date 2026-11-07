// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Preparation and presentation for the additive metadata mutation adapter.

use crate::{
    AdapterHost, HostError, InputSource, OntologyArguments, OntologyVerb, OutputDestination,
    ValidationError, decode_json_input,
};
use fabric_sdk_ontology::v2::{
    ContractError, Effect, ExecutionReceipt, FailureInfo, Outcome, Precondition, PreparedOperation,
    ReceiptBuildError, ReceiptError, Registry, ResolvedTarget, Verb,
};
use fabric_sdk_operations::OperationWarning;
use fabric_sdk_workspaces::mutations::ResourceError;
use serde_json::{Value, json};
use std::{
    fmt,
    io::{self, Write},
    num::NonZeroUsize,
};

/// Original SDK failure, retained separately from safe receipt diagnostics.
#[derive(Debug)]
pub enum CapabilityCause {
    /// Public shared-operation failure, including acceptance and mutation evidence.
    Metadata(Box<ResourceError>),
    /// Native workload failure, retaining dispatch certainty and original response.
    Native(Box<fabric_sdk_ontology_workload::workload::WorkloadError>),
}

/// Original metadata execution evidence, independent of output.
#[derive(Debug)]
pub struct CapabilityReport {
    /// Neutral receipt with the exact approved target, effects and conditions.
    pub receipt: ExecutionReceipt<Value>,
    /// Original typed SDK error; never serialized implicitly.
    pub cause: Option<CapabilityCause>,
    /// Original shared public operation warnings.
    pub warnings: Vec<OperationWarning>,
    /// Data withheld after exceeding the presentation bound, not clipped output.
    pub unpresented_data: Option<Value>,
}

impl CapabilityReport {
    /// Acceptance succeeds only as submission; partial/unknown never succeed.
    pub fn command_succeeded(&self) -> bool {
        matches!(
            self.receipt.outcome(),
            Outcome::Succeeded { .. } | Outcome::Accepted
        )
    }
}

/// Local refusal or presentation failure without hiding remote execution evidence.
#[derive(Debug)]
pub enum CapabilityError {
    /// Safe, specific local contract gap or invalid input.
    Invalid(&'static str),
    /// Original safe local verb/representation/condition validation failure.
    Validation(ValidationError),
    /// Host refused resolution, input, deadline, reservation or approval.
    Host(HostError),
    /// Canonical registry selection/binding failed.
    Contract(ContractError),
    /// Neutral effect/target/precondition validation failed.
    Preparation(ReceiptError),
    /// Inconsistent evidence, retaining the original outcome and evidence.
    Receipt(Box<ReceiptBuildError<Value>>),
    /// I/O failed after execution; never treat it as a failed provider mutation.
    Output {
        /// Original writer failure.
        source: io::Error,
        /// Original report, including handles and possible remote effects.
        report: Box<CapabilityReport>,
    },
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => f.write_str(reason),
            Self::Validation(error) => fmt::Display::fmt(error, f),
            _ => f.write_str("Ontology capability failed; inspect retained typed evidence"),
        }
    }
}
impl std::error::Error for CapabilityError {}

pub(crate) fn verb(verb: OntologyVerb) -> Verb {
    match verb {
        OntologyVerb::Create => Verb::Create,
        OntologyVerb::Delete => Verb::Delete,
        OntologyVerb::Get => Verb::Get,
        OntologyVerb::Link => Verb::Link,
        OntologyVerb::List => Verb::List,
        OntologyVerb::Move => Verb::Move,
        OntologyVerb::Update => Verb::Update,
    }
}

pub(crate) fn input<H: AdapterHost>(
    args: &OntologyArguments,
    host: &mut H,
    maximum: NonZeroUsize,
) -> Result<Option<Value>, CapabilityError> {
    args.input
        .as_ref()
        .map(|source| {
            let bytes = host
                .read_input(source, maximum)
                .map_err(CapabilityError::Host)?;
            decode_json_input(Some(&bytes), "application/json", maximum).map_err(|_| {
                CapabilityError::Invalid("Input must be one bounded strict JSON object.")
            })
        })
        .transpose()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    args: &OntologyArguments,
    facade: bool,
    registry: &Registry,
    target: ResolvedTarget,
    expected_id: &str,
    request: Value,
    required: &[Effect],
    preconditions: Vec<Precondition>,
) -> Result<PreparedOperation, CapabilityError> {
    let resolution = registry
        .resolve(
            verb(args.verb),
            target.selection(),
            facade.then_some("ontology"),
        )
        .map_err(CapabilityError::Contract)?;
    if resolution.operation.id() != expected_id {
        return Err(CapabilityError::Invalid(
            "Descriptor does not identify this SDK operation.",
        ));
    }
    if required
        .iter()
        .any(|effect| !resolution.operation.effects().contains(effect))
    {
        return Err(CapabilityError::Invalid(
            "Descriptor omits required remote effects.",
        ));
    }
    let mut optional = vec![];
    for (selected, effect) in [
        (args.input.is_some(), Effect::LocalRead),
        (
            matches!(args.output, OutputDestination::File(_)),
            Effect::LocalWrite,
        ),
    ] {
        if selected && !resolution.operation.effects().contains(&effect) {
            optional.push(effect);
        }
    }
    let output = match &args.output {
        OutputDestination::Stdout => json!({"kind":"stdout"}),
        OutputDestination::File(path) => json!({
            "kind":"file",
            "path":path.to_str().ok_or(CapabilityError::Invalid("Output path is not representable in approval evidence."))?,
        }),
    };
    let input = match &args.input {
        None => Value::Null,
        Some(InputSource::Stdin) => json!({"kind":"stdin"}),
        Some(InputSource::File(path)) => json!({
            "kind":"file",
            "path":path.to_str().ok_or(CapabilityError::Invalid("Input path is not representable in approval evidence."))?,
        }),
    };
    PreparedOperation::new(
        resolution,
        target,
        json!({
            "request":request, "input":input, "output":output, "requestedPath":args.path,
            "profile":args.profile, "wait":args.wait,
            "timeoutNanos":args.timeout.map(|value|value.as_nanos().to_string()),
        }),
    )
    .and_then(|prepared| prepared.with_optional_effects(optional))
    .and_then(|prepared| prepared.with_preconditions(preconditions))
    .map_err(CapabilityError::Preparation)
}

pub(crate) fn check_options(args: &OntologyArguments) -> Result<(), CapabilityError> {
    if args.project.is_some() {
        return Err(CapabilityError::Invalid(
            "Projection is not implemented for this adapter.",
        ));
    }
    Ok(())
}

pub(crate) fn present<H: AdapterHost>(
    host: &mut H,
    mut writer: Box<dyn Write>,
    mut report: CapabilityReport,
    approved: &fabric_sdk_ontology::v2::ApprovedOperation<'_>,
    maximum: NonZeroUsize,
) -> Result<CapabilityReport, CapabilityError> {
    if let Outcome::Succeeded { data } | Outcome::Partial { data, .. } = report.receipt.outcome()
        && serde_json::to_vec(data)
            .map_err(|_| CapabilityError::Invalid("Result serialization failed."))?
            .len()
            > maximum.get()
    {
        report.unpresented_data = Some(data.clone());
        report.receipt = ExecutionReceipt::new(
            approved,
            Outcome::Failed { error: FailureInfo {
                code: "BudgetExceeded".into(),
                message: "Returned data exceeds the serialized-data bound; no clipped source was emitted.".into(),
            }},
            report.receipt.evidence().clone(),
        ).map_err(CapabilityError::Receipt)?;
    }
    let mut bytes = serde_json::to_vec(&report.receipt)
        .map_err(|_| CapabilityError::Invalid("Receipt serialization failed."))?;
    bytes.push(b'\n');
    let result = if report.command_succeeded() {
        writer.write_all(&bytes).and_then(|_| writer.flush())
    } else {
        host.diagnostic(&bytes)
    };
    if let Err(source) = result {
        return Err(CapabilityError::Output {
            source,
            report: Box::new(report),
        });
    }
    if !report.warnings.is_empty()
        && let Err(source) = host.diagnostic(
            b"Ontology operation returned observation warnings; inspect retained evidence.\n",
        )
    {
        return Err(CapabilityError::Output {
            source,
            report: Box::new(report),
        });
    }
    Ok(report)
}
