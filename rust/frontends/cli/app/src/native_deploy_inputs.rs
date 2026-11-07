// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit binary-owned parameter inputs, never an identity or token source.

use fabric_cli::deploy::DeployInputs;
use std::{collections::BTreeMap, ffi::OsString};

/// A checked snapshot failure containing no environment names or values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum EnvironmentInputError {
    /// An environment name cannot be represented losslessly as Unicode.
    #[error("Deployment parameter environment contains a non-Unicode variable name.")]
    NonUnicodeName,
    /// An environment value cannot be represented losslessly as Unicode.
    #[error("Deployment parameter environment contains a non-Unicode variable value.")]
    NonUnicodeValue,
}

/// Convert an explicit finite iterator into parameter-only deployment inputs.
///
/// Names and values are retained exactly; duplicate names use the last value.
/// Invalid Unicode rejects the complete selection instead of silently dropping
/// an entry or returning partial inputs. No process state is read here, and
/// nothing in this map is interpreted as authentication or provider selection.
pub(crate) fn select_environment(
    entries: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<DeployInputs, EnvironmentInputError> {
    let mut environment = BTreeMap::new();
    for (name, value) in entries {
        let name = name
            .into_string()
            .map_err(|_| EnvironmentInputError::NonUnicodeName)?;
        let value = value
            .into_string()
            .map_err(|_| EnvironmentInputError::NonUnicodeValue)?;
        environment.insert(name, value);
    }
    Ok(DeployInputs { environment })
}

/// Capture the native command's process environment explicitly, without globals.
///
/// Call only at the binary's deployment-input boundary. The returned map is
/// used by the existing parameter-expansion feature flag; it must never be
/// supplied to identity selection, credentials, token providers or fallbacks.
/// This function does not log or persist the snapshot. Offline tests exercise
/// [`select_environment`] with synthetic iterators, never this process reader.
pub(crate) fn snapshot_environment() -> Result<DeployInputs, EnvironmentInputError> {
    select_environment(std::env::vars_os())
}
