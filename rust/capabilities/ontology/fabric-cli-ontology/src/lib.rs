// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Ontology facade catalog, local validation and injected public SDK execution.
//!
//! These CLI-layer descriptions are source-backed proposals, not a shared SDK
//! registry, accepted path grammar or production remote host. Common and
//! facade argument entry points use the same schema and common verb identity.
//! Parsing accesses no credentials, files or remote service. The explicit
//! execution adapter requires caller-owned resolution, reservation, approval,
//! SDK credentials/transport/runtime and finite policies before submitting work.

mod adapter;
mod args;
mod catalog;
mod input;
mod validation;

pub use adapter::{
    AdapterError, AdapterHost, AdapterLimits, ExecutionReport, HostError, bind_public_operations,
    execute,
};
pub use args::{
    ArgumentError, InputSource, OntologyArguments, OutputDestination, UpdateMode, ontology_command,
    operation_command, parse_common_args, parse_facade_args,
};
pub use catalog::{
    Backend, BodyContract, Effect, GenerationSupport, OntologyBinding, OntologyTarget,
    OntologyVerb, SelectionError, bindings, select_binding,
};
pub use input::{InputError, decode_json_input};
pub use validation::{ValidationError, validate_arguments};
