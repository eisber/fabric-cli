// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Ontology facade catalog and local argument validation.
//!
//! These CLI-layer descriptions are source-backed proposals, not a shared SDK
//! registry, accepted path grammar or executable remote capability. Common and
//! facade argument entry points use the same schema and common verb identity.
//! No credentials, filesystem, HTTP, runtime or operation submission are accessed.

mod args;
mod catalog;
mod validation;

pub use args::{
    ArgumentError, InputSource, OntologyArguments, OutputDestination, UpdateMode, ontology_command,
    operation_command, parse_common_args, parse_facade_args,
};
pub use catalog::{
    Backend, BodyContract, Effect, GenerationSupport, OntologyBinding, OntologyTarget,
    OntologyVerb, SelectionError, bindings, select_binding,
};
pub use validation::{ValidationError, validate_arguments};
