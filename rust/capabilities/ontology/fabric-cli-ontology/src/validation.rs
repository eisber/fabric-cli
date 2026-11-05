// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure argument/representation compatibility, not execution preflight.

use crate::{
    Backend, BodyContract, OntologyArguments, OntologyBinding, OntologyTarget, OntologyVerb,
    SelectionError, UpdateMode, select_binding,
};
use std::fmt;

/// Local incompatibility discovered before any credential or host access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// The resolved type/generation/representation is not admitted.
    Selection(SelectionError),
    /// The body contract requires explicit local input.
    InputRequired,
    /// A body is not part of this operation's contract.
    UnexpectedInput,
    /// Patch/replace does not match the explicitly selected representation.
    UnsupportedUpdateMode,
    /// Native schema deletion is hard-only and needs explicit permanent intent.
    PermanentIntentRequired,
    /// This source binding has no supported client If-Match contract.
    UnsupportedCondition,
    /// A synchronous binding has no accepted-operation observation contract.
    UnsupportedWait,
    /// No exactly bounded recursive deletion contract is installed.
    UnsupportedRecursion,
    /// Link/move need accepted native input and effect contracts.
    UnresolvedCommandContract,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Selection(error) => fmt::Display::fmt(error, f),
            Self::InputRequired => {
                f.write_str("This representation requires explicit typed input.")
            }
            Self::UnexpectedInput => f.write_str("This operation does not accept an input body."),
            Self::UnsupportedUpdateMode => {
                f.write_str("The update mode is unsupported for this representation.")
            }
            Self::PermanentIntentRequired => {
                f.write_str("This hard-only target requires explicit permanent intent.")
            }
            Self::UnsupportedCondition => {
                f.write_str("This binding does not support a client revision condition.")
            }
            Self::UnsupportedWait => {
                f.write_str("This binding does not support operation observation.")
            }
            Self::UnsupportedRecursion => {
                f.write_str("This binding has no bounded recursive deletion contract.")
            }
            Self::UnresolvedCommandContract => {
                f.write_str("The native binding/rename contract is not accepted.")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Checks local arguments against a source-backed representation proposal.
///
/// Success is only local compatibility: the caller must still validate typed
/// body/media/size/projection, bind canonical identities and finite budgets,
/// reserve output, obtain exact host approval, and check producer/backend gates.
/// This function cannot authorize remote work or manufacture a typed receipt.
///
/// # Errors
/// Rejects unsupported selection, input/mode, revision, wait and deletion intent.
pub fn validate_arguments(
    args: &OntologyArguments,
    target: OntologyTarget,
    owner_type: &str,
    generation: Option<u64>,
) -> Result<&'static OntologyBinding, ValidationError> {
    let binding = select_binding(args.verb, target, owner_type, generation)
        .map_err(ValidationError::Selection)?;
    if matches!(args.verb, OntologyVerb::Link | OntologyVerb::Move) {
        return Err(ValidationError::UnresolvedCommandContract);
    }
    if args.if_match.is_some() {
        return Err(ValidationError::UnsupportedCondition);
    }
    if args.recursive {
        return Err(ValidationError::UnsupportedRecursion);
    }
    if args.verb == OntologyVerb::Delete && target == OntologyTarget::Entity && !args.permanent {
        return Err(ValidationError::PermanentIntentRequired);
    }
    if binding.body == BodyContract::None {
        if args.input.is_some() {
            return Err(ValidationError::UnexpectedInput);
        }
    } else if args.input.is_none() {
        return Err(ValidationError::InputRequired);
    }
    let required_mode = match binding.body {
        BodyContract::MetadataPatch => Some(UpdateMode::Patch),
        BodyContract::DefinitionReplace
        | BodyContract::PublicModelReplace
        | BodyContract::NativeModelReplace => Some(UpdateMode::Replace),
        _ => None,
    };
    if args.mode != required_mode {
        return Err(ValidationError::UnsupportedUpdateMode);
    }
    let can_observe = binding.backend == Backend::PublicDefinition
        || (args.verb == OntologyVerb::Create && target == OntologyTarget::Item);
    if args.wait && !can_observe {
        return Err(ValidationError::UnsupportedWait);
    }
    Ok(binding)
}
