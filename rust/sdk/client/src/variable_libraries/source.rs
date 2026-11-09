// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Error, SourceDisclosure};
use crate::representations::{DefinitionRead, RepresentationLimits};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, fmt, num::NonZeroUsize};

const VARIABLES_SCHEMA: &str = "https://developer.microsoft.com/json-schemas/fabric/item/variableLibrary/definition/variables/1.0.0/schema.json";
const SETTINGS_SCHEMA: &str = "https://developer.microsoft.com/json-schemas/fabric/item/variableLibrary/definition/settings/1.0.0/schema.json";
const VALUE_SET_SCHEMA: &str = "https://developer.microsoft.com/json-schemas/fabric/item/variableLibrary/definition/valueSet/1.0.0/schema.json";

/// Caller-owned finite bounds, distinct from service authoring limits.
#[derive(Clone, Copy, Debug)]
pub struct SourceLimits {
    /// Existing whole-envelope HTTP and compact JSON bounds, before projection.
    pub representation: RepresentationLimits,
    /// Maximum complete source parts, including defaults, settings and platform.
    pub max_parts: NonZeroUsize,
    /// Maximum aggregate decoded bytes across all native source parts.
    pub max_decoded_bytes: NonZeroUsize,
}

/// Variable shape, not its default value or effective consumer value.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableShape {
    /// Native variable name, preserving spelling.
    pub name: String,
    /// Native type name, without guessed casing normalization or closed enum.
    pub variable_type: String,
    /// Whether source contains a default value, including explicit null.
    pub has_default_value: bool,
}

/// One alternate value-set shape, without values, descriptions or encoded data.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueSetShape {
    /// Name from native content, required to match the exact file stem.
    pub name: String,
    /// Number of explicit overrides, not a count of nonempty/truthy values.
    pub override_count: usize,
    /// Distinguishes an omitted override list from an explicitly empty list.
    pub has_override_list: bool,
}

/// Native shapes in source order; never a runtime-resolved value snapshot.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceShape {
    /// Default variable declarations in native source order.
    pub variables: Vec<VariableShape>,
    /// Alternate value sets in multipart order, not active or display order.
    pub value_sets: Vec<ValueSetShape>,
    /// Explicit source display ordering, not active-set selection.
    pub value_sets_order: Option<Vec<String>>,
}

macro_rules! redacted_debug {
    ($($name:ty),+ $(,)?) => {$(
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    )+};
}

redacted_debug!(VariableShape, ValueSetShape, SourceShape);

/// A completed source read with original multipart data and value-free shapes.
///
/// This deliberately has no Serialize implementation: source disclosure must be
/// an explicit accessor/output choice, never incidental receipt serialization.
pub struct SourceDefinition {
    native: DefinitionRead,
    shape: SourceShape,
}

impl SourceDefinition {
    /// Explicitly borrows original parsed source, including all encoded values.
    ///
    /// Preserve part paths, order and payload bytes for native round trips.
    /// This does not grant filesystem writes or source publication.
    pub fn native_definition(&self) -> &Value {
        match &self.native {
            DefinitionRead::Completed { definition } => definition,
            DefinitionRead::Accepted { .. } => unreachable!("completed source invariant"),
        }
    }

    /// Borrows value-free shapes derived from the entire disclosed source.
    ///
    /// Defaults, overrides, notes, descriptions, unknown content and active values
    /// are not serialized into the shape. Names/type tokens remain source data.
    pub fn shape(&self) -> &SourceShape {
        &self.shape
    }
}

redacted_debug!(SourceDefinition);

/// Actual source completion or acceptance, with no fabricated empty shape.
pub enum SourceRead {
    /// Completed and structurally checked native source.
    Completed(SourceDefinition),
    /// Accepted operation, scoped to the same client context that requested it.
    Accepted {
        /// Actual validated operation ID, not replay or observation authority.
        operation_id: String,
    },
}

redacted_debug!(SourceRead);

impl SourceRead {
    /// Validates an actually completed operation result using the same codec.
    ///
    /// The caller must establish completion and bind these bytes to the original
    /// client/target/operation. This function performs no observation or requests.
    ///
    /// # Errors
    /// Rejects incomplete/malformed/unsupported source and explicit budget limits.
    pub fn from_completed_result(
        body: &[u8],
        _disclosure: SourceDisclosure,
        limits: SourceLimits,
    ) -> Result<Self, Error> {
        let native = DefinitionRead::from_completed_result(body, limits.representation)
            .map_err(Error::Source)?;
        Self::decode(native, limits)
    }

    /// Returns an accepted handle for the existing shared observer, if needed.
    ///
    /// Use the same client's immutable endpoint/tenant/credential context.
    /// This accessor does not poll, cancel, replay or establish completion.
    pub fn accepted_handle(&self) -> Option<crate::observe::ReadHandle> {
        match self {
            Self::Accepted { operation_id } => Some(crate::observe::ReadHandle::Operation {
                operation_id: operation_id.clone(),
            }),
            Self::Completed(_) => None,
        }
    }

    pub(super) fn decode(native: DefinitionRead, limits: SourceLimits) -> Result<Self, Error> {
        if let DefinitionRead::Accepted { operation_id } = native {
            return Ok(Self::Accepted { operation_id });
        }
        let shape = decode_shape(&native, limits)?;
        Ok(Self::Completed(SourceDefinition { native, shape }))
    }
}

fn decode_shape(native: &DefinitionRead, limits: SourceLimits) -> Result<SourceShape, Error> {
    let DefinitionRead::Completed { definition } = native else {
        return Err(Error::InvalidSource);
    };
    let definition = &definition["definition"];
    if definition
        .get("format")
        .is_some_and(|format| format.as_str() != Some("VariableLibraryV1"))
    {
        return Err(Error::UnsupportedSource);
    }
    let parts = definition["parts"].as_array().ok_or(Error::InvalidSource)?;
    if parts.len() > limits.max_parts.get() {
        return Err(Error::BudgetExceeded);
    }
    let mut paths = BTreeSet::new();
    let mut remaining = limits.max_decoded_bytes.get();
    let mut variables = None;
    let mut settings = None;
    let mut sets = Vec::new();
    for part in parts {
        let path = part["path"].as_str().ok_or(Error::InvalidSource)?;
        if !paths.insert(path) {
            return Err(Error::InvalidSource);
        }
        let set_name = match path {
            "variables.json" | "settings.json" | ".platform" => None,
            _ => Some(
                path.strip_prefix("valueSets/")
                    .and_then(|path| path.strip_suffix(".json"))
                    .filter(|name| valid_container_name(name))
                    .ok_or(Error::UnsupportedSource)?,
            ),
        };
        let payload = part["payload"].as_str().ok_or(Error::InvalidSource)?;
        if payload.len() / 4 * 3 > remaining.saturating_add(2) {
            return Err(Error::BudgetExceeded);
        }
        let bytes = native.part_bytes(path).map_err(Error::Source)?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or(Error::BudgetExceeded)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidSource)?;
        if !value.is_object() {
            return Err(Error::InvalidSource);
        }
        let schema = match path {
            "variables.json" => Some(VARIABLES_SCHEMA),
            "settings.json" => Some(SETTINGS_SCHEMA),
            ".platform" => None,
            _ => Some(VALUE_SET_SCHEMA),
        };
        if schema.is_some_and(|schema| value["$schema"].as_str() != Some(schema)) {
            return Err(Error::UnsupportedSource);
        }
        match path {
            "variables.json" => variables = Some(value),
            "settings.json" => settings = Some(value),
            ".platform" => {}
            _ => sets.push((set_name.ok_or(Error::InvalidSource)?, value)),
        }
    }
    let variables = variables.ok_or(Error::InvalidSource)?;
    let mut names = BTreeSet::new();
    let mut shape = SourceShape {
        variables: Vec::new(),
        value_sets: Vec::new(),
        value_sets_order: None,
    };
    for variable in variables["variables"]
        .as_array()
        .ok_or(Error::InvalidSource)?
    {
        let name = name(&variable["name"])?;
        let variable_type = variable["type"].as_str().ok_or(Error::InvalidSource)?;
        if !names.insert(name.to_ascii_lowercase())
            || variable_type.is_empty()
            || variable_type.len() > 64
            || !variable_type.as_bytes()[0].is_ascii_alphabetic()
            || !variable_type
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
            || variable.get("note").is_some_and(|note| !note.is_string())
        {
            return Err(Error::InvalidSource);
        }
        shape.variables.push(VariableShape {
            name: name.into(),
            variable_type: variable_type.into(),
            has_default_value: variable.get("value").is_some(),
        });
    }
    let mut set_names = BTreeSet::new();
    for (stem, value) in sets {
        let set_name = container_name(&value["name"])?;
        if stem != set_name
            || !set_names.insert(set_name.to_owned())
            || value
                .get("description")
                .is_some_and(|description| !description.is_string())
        {
            return Err(Error::InvalidSource);
        }
        let overrides = match value.get("variableOverrides") {
            None => &[][..],
            Some(value) => value.as_array().ok_or(Error::InvalidSource)?.as_slice(),
        };
        let mut overridden = BTreeSet::new();
        for value in overrides {
            let variable = name(&value["name"])?.to_ascii_lowercase();
            if !names.contains(&variable)
                || !overridden.insert(variable)
                || value.get("value").is_none()
            {
                return Err(Error::InvalidSource);
            }
        }
        shape.value_sets.push(ValueSetShape {
            name: set_name.into(),
            override_count: overrides.len(),
            has_override_list: value.get("variableOverrides").is_some(),
        });
    }
    let settings = settings.ok_or(Error::InvalidSource)?;
    if let Some(order) = settings.get("valueSetsOrder") {
        let mut ordered = BTreeSet::new();
        let mut order_names = Vec::new();
        for value in order.as_array().ok_or(Error::InvalidSource)? {
            let value = container_name(value)?;
            if !set_names.contains(value) || !ordered.insert(value) {
                return Err(Error::InvalidSource);
            }
            order_names.push(value.into());
        }
        shape.value_sets_order = Some(order_names);
    }
    Ok(shape)
}

fn name(value: &Value) -> Result<&str, Error> {
    value
        .as_str()
        .filter(|value| valid_name(value, false))
        .ok_or(Error::InvalidSource)
}

fn container_name(value: &Value) -> Result<&str, Error> {
    value
        .as_str()
        .filter(|value| valid_container_name(value))
        .ok_or(Error::InvalidSource)
}

fn valid_container_name(value: &str) -> bool {
    valid_name(value, true)
}

fn valid_name(value: &str, allow_spaces: bool) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && (value.as_bytes()[0].is_ascii_alphabetic() || value.starts_with('_'))
        && !value.ends_with(' ')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-')
                || (allow_spaces && byte == b' ')
        })
}
