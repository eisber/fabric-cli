// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native whole-Notebook request validation, separate from legacy `RunNotebook`.
//!
//! The release API uses top-level parameters and compute-discriminated
//! execution data. Validation is pure: it never reads Notebook definitions,
//! resolves pools, downloads libraries or acquires credentials.

use serde_json::{Map, Value};
use std::{collections::HashSet, fmt};

mod execution;
pub use execution::execute;

/// Native Notebook compute discriminator; configurations are not interchangeable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotebookCompute {
    /// Spark execution, including its own resource and library configuration.
    Spark,
    /// Jupyter execution, with its separate core and mount configuration.
    Jupyter,
    /// Data Warehouse execution, without compute configuration.
    DataWarehouse,
}

/// A supported native parameter type, without legacy conversion or coercion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotebookParameterType {
    /// JSON string.
    Text,
    /// JSON integer, never a rounded float or parsed string.
    Integer,
    /// Finite JSON number, retaining its original integer or float representation.
    Number,
    /// JSON boolean.
    Boolean,
    /// UTC calendar timestamp in `YYYY-MM-DDTHH:mm:ssZ` format.
    DateTime,
    /// ASCII hexadecimal GUID in `8-4-4-4-12` format.
    Guid,
}

/// Locally rejected input; diagnostics never contain supplied parameter values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NotebookInputError {
    /// An object contains an unknown or unsupported field.
    #[error("unsupported native Notebook request field")]
    UnsupportedField,
    /// A required field is missing or has a different native JSON type.
    #[error("invalid native Notebook request shape or value type")]
    InvalidShape,
    /// Parameter names must be unique case-insensitively and at most 256 characters.
    #[error("invalid or duplicate native Notebook parameter name")]
    ParameterName,
    /// Automatic, VariableReference and future types have no accepted value contract.
    #[error("native Notebook parameter type semantics are unavailable")]
    UnsupportedParameterType,
    /// The parameter value does not match its declared type or documented format.
    #[error("native Notebook parameter value does not match its declared type")]
    ParameterValue,
    /// The compute discriminator is not a supported native compute type.
    #[error("unsupported native Notebook compute type")]
    UnsupportedCompute,
    /// A resource value lies outside the documented enum or local numeric range.
    #[error("invalid native Notebook compute resource configuration")]
    ComputeResource,
    /// A reference does not have the documented ById GUID shape.
    #[error("unsupported or invalid native Notebook item reference")]
    ItemReference,
    /// A native file reference does not have its documented path or extension.
    #[error("invalid native Notebook storage reference")]
    StorageReference,
}

/// Validated native request, preserving no input versus an explicit empty object.
///
/// Instances can only be constructed through local validation. The exact object,
/// including ordering, omissions, name casing and number representation, is
/// retained. It must be submitted as the entire request body, never wrapped in
/// another `executionData` object or passed to legacy [`crate::JobClient::submit`].
#[derive(Clone, Eq, PartialEq)]
pub struct NotebookRunRequest {
    body: Option<Value>,
    compute: Option<NotebookCompute>,
}

impl NotebookRunRequest {
    /// Validate native JSON without effects or value coercion.
    ///
    /// `None` means zero body bytes. `Some({})` means an explicit JSON object.
    /// Null, legacy parameter maps, unknown fields and unavailable parameter
    /// semantics are rejected rather than inferred. Pool-dependent executor
    /// maxima and agreement with saved Notebook language remain service checks.
    pub fn from_input(input: Option<Value>) -> Result<Self, NotebookInputError> {
        let compute = match input.as_ref() {
            None => None,
            Some(value) => {
                let body = object(value, &["parameters", "executionData"])?;
                if let Some(parameters) = body.get("parameters") {
                    validate_parameters(parameters)?;
                }
                body.get("executionData")
                    .map(validate_execution_data)
                    .transpose()?
            }
        };
        Ok(Self {
            body: input,
            compute,
        })
    }

    /// Exact validated top-level body, or None for a bodyless submission.
    pub fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }

    /// Explicit native compute, absent when the service selects saved defaults.
    pub const fn compute(&self) -> Option<NotebookCompute> {
        self.compute
    }

    /// Validate the released execution adapter's non-high-concurrency profile.
    ///
    /// The schema validator retains the full documented request contract, but
    /// high-concurrency session reuse is not installed by the Run adapter.
    ///
    /// # Errors
    /// Rejects explicit high-concurrency options without changing the payload.
    pub fn validate_standard_execution(&self) -> Result<(), NotebookInputError> {
        if self
            .body
            .as_ref()
            .and_then(|body| {
                body.pointer("/executionData/computeConfiguration/highConcurrencyModeOptions")
            })
            .is_some()
        {
            return Err(NotebookInputError::UnsupportedField);
        }
        Ok(())
    }
}

impl fmt::Debug for NotebookRunRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotebookRunRequest")
            .field("body_present", &self.body.is_some())
            .field("compute", &self.compute)
            .finish_non_exhaustive()
    }
}

fn object<'a>(
    value: &'a Value,
    fields: &[&str],
) -> Result<&'a Map<String, Value>, NotebookInputError> {
    let object = value.as_object().ok_or(NotebookInputError::InvalidShape)?;
    if object.keys().any(|name| !fields.contains(&name.as_str())) {
        return Err(NotebookInputError::UnsupportedField);
    }
    Ok(object)
}

fn text(value: &Value) -> Result<&str, NotebookInputError> {
    value.as_str().ok_or(NotebookInputError::InvalidShape)
}

fn validate_parameters(value: &Value) -> Result<(), NotebookInputError> {
    let parameters = value.as_array().ok_or(NotebookInputError::InvalidShape)?;
    let mut names = HashSet::new();
    for value in parameters {
        let parameter = object(value, &["name", "type", "value"])?;
        let name = parameter
            .get("name")
            .and_then(Value::as_str)
            .ok_or(NotebookInputError::InvalidShape)?;
        if name.chars().count() > 256 || !names.insert(name.to_lowercase()) {
            return Err(NotebookInputError::ParameterName);
        }
        let parameter_type = match parameter.get("type").and_then(Value::as_str) {
            Some("Text") => NotebookParameterType::Text,
            Some("Integer") => NotebookParameterType::Integer,
            Some("Number") => NotebookParameterType::Number,
            Some("Boolean") => NotebookParameterType::Boolean,
            Some("DateTime") => NotebookParameterType::DateTime,
            Some("Guid") => NotebookParameterType::Guid,
            Some(_) => return Err(NotebookInputError::UnsupportedParameterType),
            None => return Err(NotebookInputError::InvalidShape),
        };
        let value = parameter
            .get("value")
            .ok_or(NotebookInputError::InvalidShape)?;
        let valid = match parameter_type {
            NotebookParameterType::Text => value.is_string(),
            NotebookParameterType::Integer => value.is_i64() || value.is_u64(),
            NotebookParameterType::Number => value.as_f64().is_some_and(f64::is_finite),
            NotebookParameterType::Boolean => value.is_boolean(),
            NotebookParameterType::DateTime => value.as_str().is_some_and(valid_datetime),
            NotebookParameterType::Guid => value.as_str().is_some_and(valid_guid),
        };
        if !valid {
            return Err(NotebookInputError::ParameterValue);
        }
    }
    Ok(())
}

fn validate_execution_data(value: &Value) -> Result<NotebookCompute, NotebookInputError> {
    let data = object(value, &["compute", "computeConfiguration"])?;
    let compute = match data.get("compute").and_then(Value::as_str) {
        Some("Spark") => NotebookCompute::Spark,
        Some("Jupyter") => NotebookCompute::Jupyter,
        Some("DataWarehouse") => NotebookCompute::DataWarehouse,
        Some(_) => return Err(NotebookInputError::UnsupportedCompute),
        None => return Err(NotebookInputError::InvalidShape),
    };
    if let Some(configuration) = data.get("computeConfiguration") {
        match compute {
            NotebookCompute::Spark => validate_spark(configuration)?,
            NotebookCompute::Jupyter => validate_jupyter(configuration)?,
            NotebookCompute::DataWarehouse => {
                return Err(NotebookInputError::UnsupportedField);
            }
        }
    }
    Ok(compute)
}

fn validate_jupyter(value: &Value) -> Result<(), NotebookInputError> {
    let configuration = object(
        value,
        &[
            "name",
            "numCores",
            "mountPoints",
            "defaultLakehouse",
            "attachedEnvironment",
        ],
    )?;
    if let Some(cores) = configuration.get("numCores") {
        cores_enum(cores, &[2, 4, 8, 16, 32, 64])?;
    }
    validate_common_configuration(configuration)
}

fn validate_spark(value: &Value) -> Result<(), NotebookInputError> {
    let configuration = object(
        value,
        &[
            "name",
            "driverMemory",
            "driverCores",
            "executorMemory",
            "executorCores",
            "numExecutors",
            "jars",
            "pyFiles",
            "files",
            "archives",
            "sparkProperties",
            "instancePool",
            "mountPoints",
            "defaultLakehouse",
            "attachedEnvironment",
            "highConcurrencyModeOptions",
        ],
    )?;
    for name in ["driverCores", "executorCores"] {
        if let Some(cores) = configuration.get(name) {
            cores_enum(cores, &[4, 8, 16, 32, 64])?;
        }
    }
    for name in ["driverMemory", "executorMemory"] {
        if let Some(memory) = configuration.get(name)
            && !memory
                .as_str()
                .is_some_and(|memory| ["28g", "56g", "112g", "224g", "400g"].contains(&memory))
        {
            return Err(NotebookInputError::ComputeResource);
        }
    }
    if let Some(count) = configuration.get("numExecutors")
        && !count
            .as_u64()
            .is_some_and(|count| count >= 1 && count <= i32::MAX as u64)
    {
        return Err(NotebookInputError::ComputeResource);
    }
    for (name, extensions) in [
        ("jars", &[][..]),
        ("files", &[][..]),
        ("pyFiles", &[".zip", ".egg", ".py"][..]),
        ("archives", &[".jar", ".tar.gz", ".tgz", ".zip"][..]),
    ] {
        if let Some(paths) = configuration.get(name) {
            for path in paths.as_array().ok_or(NotebookInputError::InvalidShape)? {
                storage_path(text(path)?, extensions)?;
            }
        }
    }
    if let Some(properties) = configuration.get("sparkProperties") {
        for property in properties
            .as_array()
            .ok_or(NotebookInputError::InvalidShape)?
        {
            let property = object(property, &["key", "value"])?;
            for name in ["key", "value"] {
                text(property.get(name).ok_or(NotebookInputError::InvalidShape)?)?;
            }
        }
    }
    if let Some(pool) = configuration.get("instancePool") {
        // maxClustersToHydrateLimit is server-computed, not a client override.
        let pool = object(pool, &["id", "name", "type"])?;
        if let Some(id) = pool.get("id")
            && !id.as_str().is_some_and(valid_guid)
        {
            return Err(NotebookInputError::ItemReference);
        }
        if let Some(name) = pool.get("name") {
            text(name)?;
        }
        if let Some(pool_type) = pool.get("type")
            && !matches!(pool_type.as_str(), Some("Workspace" | "Capacity"))
        {
            return Err(NotebookInputError::ComputeResource);
        }
    }
    if let Some(options) = configuration.get("highConcurrencyModeOptions") {
        let options = object(options, &["enabled", "sessionTag"])?;
        if let Some(enabled) = options.get("enabled")
            && !enabled.is_boolean()
        {
            return Err(NotebookInputError::InvalidShape);
        }
        if let Some(tag) = options.get("sessionTag") {
            text(tag)?;
        }
    }
    validate_common_configuration(configuration)
}

fn validate_common_configuration(
    configuration: &Map<String, Value>,
) -> Result<(), NotebookInputError> {
    if let Some(name) = configuration.get("name") {
        text(name)?;
    }
    for name in ["defaultLakehouse", "attachedEnvironment"] {
        if let Some(reference) = configuration.get(name) {
            let reference = object(reference, &["referenceType", "itemId", "workspaceId"])?;
            if reference.get("referenceType").and_then(Value::as_str) != Some("ById")
                || ["itemId", "workspaceId"].into_iter().any(|name| {
                    !reference
                        .get(name)
                        .and_then(Value::as_str)
                        .is_some_and(valid_guid)
                })
            {
                return Err(NotebookInputError::ItemReference);
            }
        }
    }
    if let Some(mounts) = configuration.get("mountPoints") {
        for mount in mounts.as_array().ok_or(NotebookInputError::InvalidShape)? {
            let mount = object(mount, &["source", "mountPointPath"])?;
            storage_path(
                text(
                    mount
                        .get("source")
                        .ok_or(NotebookInputError::InvalidShape)?,
                )?,
                &[],
            )?;
            text(
                mount
                    .get("mountPointPath")
                    .ok_or(NotebookInputError::InvalidShape)?,
            )?;
        }
    }
    Ok(())
}

fn cores_enum(value: &Value, cores: &[u64]) -> Result<(), NotebookInputError> {
    if !value.as_u64().is_some_and(|value| cores.contains(&value)) {
        return Err(NotebookInputError::ComputeResource);
    }
    Ok(())
}

fn storage_path(path: &str, extensions: &[&str]) -> Result<(), NotebookInputError> {
    let url =
        azure_core::http::Url::parse(path).map_err(|_| NotebookInputError::StorageReference)?;
    if !matches!(url.scheme(), "abfs" | "abfss")
        || url.host_str().is_none()
        || url.password().is_some()
        || url.query().is_some()
        || (!extensions.is_empty()
            && !extensions
                .iter()
                .any(|extension| url.path().ends_with(extension)))
    {
        return Err(NotebookInputError::StorageReference);
    }
    Ok(())
}

fn valid_guid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn valid_datetime(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || !bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| {
        bytes[range]
            .iter()
            .fold(0_u32, |value, byte| value * 10 + u32::from(byte - b'0'))
    };
    let year = number(0..4);
    let month = number(5..7);
    let day = number(8..10);
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    year >= 1
        && (1..=days).contains(&day)
        && number(11..13) <= 23
        && number(14..16) <= 59
        && number(17..19) <= 59
}
