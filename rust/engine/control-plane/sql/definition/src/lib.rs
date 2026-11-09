// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure, bounded SQLDatabase native definition admission and serialization.
//!
//! The caller selects all limits. Native paths, bytes and part order are retained;
//! admission grants no local filesystem authority or package validity guarantee.
//! No HTTP, credentials, runtime, filesystem writer or deployment workflow lives
//! here. Service status, approval, operation evidence and output policy belong to
//! callers. In particular, permission denial is not a metadata-only definition.

#![deny(unnameable_test_items)]

mod codec;
mod error;
mod model;
mod paths;
pub mod validation;
mod wire;

pub use codec::{
    parse_sql_database_definition, parse_sql_database_definition_response,
    sql_database_definition_from_parts,
};
pub use error::{DefinitionError, DefinitionErrorKind, DefinitionLimitKind};
pub use model::{
    DefinitionLimits, NativeDefinition, NativeDefinitionPart, SqlDatabaseDefinitionFormat,
};
