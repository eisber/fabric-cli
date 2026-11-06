// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Ontology facade catalog, validation and injected public/native SDK execution.
//!
//! These CLI-layer descriptions are source-backed proposals, not a shared SDK
//! registry, accepted path grammar or production remote host. Common and
//! facade argument entry points use the same schema and common verb identity.
//! Parsing accesses no credentials, files or remote service. The explicit
//! execution adapters require caller-owned resolution, reservation, approval,
//! SDK credentials/transport/runtime and finite policies before submitting work.
//!
//! [`execute_metadata`] composes metadata-only item Create/Patch/Delete through
//! the existing shared public operation session. [`execute_native`] implements
//! typed generation-2 entity CRUDL, binding and rename with an explicit native
//! host. Neither mounts root commands, discovers a host or manages DataAgents.

mod adapter;
mod args;
mod catalog;
mod execution_support;
mod input;
mod metadata_adapter;
mod native;
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
pub use execution_support::{CapabilityCause, CapabilityError, CapabilityReport};
pub use input::{InputError, decode_json_input};
pub use metadata_adapter::{bind_metadata_operations, execute_metadata};
pub use native::{
    NativeBinding, NativeContext, NativeHost, bind_native_operations, execute_native,
};
pub use validation::{ValidationError, validate_arguments};
