// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::DiscoveryError;
use serde_json::{Map, Value, json};

pub(crate) const UUID_PATTERN: &str =
    "^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$";

/// The deliberately finite constraint AST shared by every projection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StringConstraint {
    pub(crate) max_length: Option<usize>,
    pub(crate) uuid: bool,
    pub(crate) choices: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Field {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) required: bool,
    pub(crate) nullable: bool,
    pub(crate) default: Option<&'static str>,
    pub(crate) constraint: StringConstraint,
}

#[derive(Debug)]
pub(crate) struct ObjectConstraint {
    pub(crate) fields: &'static [Field],
    /// At most one field from each group may be present, including null values.
    pub(crate) exclusions: &'static [&'static [&'static str]],
}

#[derive(Debug)]
pub(crate) enum InputConstraint {
    Required(&'static ObjectConstraint),
    Absent {
        /// A known missing parameter signature, rather than a no-payload API.
        parameter_gap: bool,
    },
}

impl InputConstraint {
    pub(crate) fn required(&self) -> bool {
        matches!(self, Self::Required(_))
    }

    pub(crate) fn schema(&self) -> Value {
        match self {
            Self::Required(object) => object.schema(),
            Self::Absent { .. } => Value::Bool(false),
        }
    }

    pub(crate) fn validate(&self, input: Option<&Value>) -> Result<(), DiscoveryError> {
        match (self, input) {
            (Self::Required(object), Some(value)) => object.validate(value),
            (Self::Required(_), None) => Err(invalid("$", "a JSON object body is required")),
            (Self::Absent { .. }, None) => Ok(()),
            (
                Self::Absent {
                    parameter_gap: true,
                },
                Some(_),
            ) => Err(DiscoveryError::UnsupportedCapability {
                reason: "Notebook run parameters and executionData have no registered exact signature",
            }),
            (Self::Absent { .. }, Some(_)) => {
                Err(invalid("$", "this operation has no request body"))
            }
        }
    }

    pub(crate) fn help(&self) -> String {
        match self {
            Self::Required(object) => object.help(),
            Self::Absent {
                parameter_gap: true,
            } => "Input: absent only; Notebook executionData and parameters are unsupported (exact signature unavailable).".into(),
            Self::Absent { .. } => {
                "Input: absent only; null and an empty object are not absence.".into()
            }
        }
    }
}

impl ObjectConstraint {
    fn schema(&self) -> Value {
        let properties: Map<_, _> = self
            .fields
            .iter()
            .map(|field| (field.name.to_owned(), field.schema()))
            .collect();
        let required: Vec<_> = self
            .fields
            .iter()
            .filter(|field| field.required)
            .map(|field| field.name)
            .collect();
        let mut exclusions = Vec::new();
        for group in self.exclusions {
            for (index, first) in group.iter().enumerate() {
                for second in &group[index + 1..] {
                    exclusions.push(json!({"not": {"required": [first, second]}}));
                }
            }
        }
        let mut schema = json!({
            "$schema": crate::INPUT_SCHEMA_DIALECT,
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        });
        if !exclusions.is_empty() {
            schema["allOf"] = json!(exclusions);
        }
        schema
    }

    fn validate(&self, input: &Value) -> Result<(), DiscoveryError> {
        let object = input
            .as_object()
            .ok_or_else(|| invalid("$", "expected a JSON object, not null or another JSON type"))?;
        if object
            .keys()
            .any(|name| !self.fields.iter().any(|field| field.name == name.as_str()))
        {
            return Err(invalid(
                "$",
                "unknown field; only registered fields are supported",
            ));
        }
        for field in self.fields {
            match object.get(field.name) {
                None if field.required => {
                    return Err(invalid(field.name, "required field is absent"));
                }
                None => {}
                Some(value) => field.validate(value)?,
            }
        }
        if self.exclusions.iter().any(|group| {
            group
                .iter()
                .filter(|name| object.contains_key(**name))
                .count()
                > 1
        }) {
            return Err(invalid("$", "mutually exclusive fields are present"));
        }
        Ok(())
    }

    fn help(&self) -> String {
        let mut lines =
            vec!["Input: required JSON object; unknown fields are rejected.".to_owned()];
        for field in self.fields {
            let mut line = format!(
                "  {}: string, {}{}",
                field.name,
                if field.required {
                    "required"
                } else {
                    "optional"
                },
                if field.nullable {
                    ", nullable"
                } else {
                    ", non-null"
                }
            );
            if let Some(max) = field.constraint.max_length {
                line.push_str(&format!(", maxLength={max}"));
            }
            if field.constraint.uuid {
                line.push_str(", UUID (8-4-4-4-12 hexadecimal)");
            }
            if !field.constraint.choices.is_empty() {
                line.push_str(&format!(", enum={:?}", field.constraint.choices));
            }
            if let Some(default) = field.default {
                line.push_str(&format!(", default={default:?} (annotation; not applied)"));
            }
            line.push_str(&format!(". {}", field.description));
            lines.push(line);
        }
        for group in self.exclusions {
            lines.push(format!("  At most one present: {}.", group.join(", ")));
        }
        lines.join("\n")
    }
}

impl Field {
    fn schema(self) -> Value {
        let mut schema = json!({
            "type": if self.nullable { json!(["string", "null"]) } else { json!("string") },
            "description": self.description
        });
        if let Some(max) = self.constraint.max_length {
            schema["maxLength"] = json!(max);
        }
        if self.constraint.uuid {
            schema["format"] = json!("uuid");
            schema["pattern"] = json!(UUID_PATTERN);
        }
        if !self.constraint.choices.is_empty() {
            let mut choices: Vec<_> = self.constraint.choices.iter().map(|s| json!(s)).collect();
            if self.nullable {
                choices.push(Value::Null);
            }
            schema["enum"] = json!(choices);
        }
        if let Some(default) = self.default {
            schema["default"] = json!(default);
        }
        schema
    }

    fn validate(self, value: &Value) -> Result<(), DiscoveryError> {
        if self.nullable && value.is_null() {
            return Ok(());
        }
        let text = value
            .as_str()
            .ok_or_else(|| invalid(self.name, "expected a string"))?;
        if self
            .constraint
            .max_length
            .is_some_and(|max| text.chars().count() > max)
        {
            return Err(invalid(self.name, "string exceeds maxLength"));
        }
        if self.constraint.uuid && !is_uuid(text) {
            return Err(invalid(
                self.name,
                "expected an 8-4-4-4-12 hexadecimal UUID",
            ));
        }
        if !self.constraint.choices.is_empty() && !self.constraint.choices.contains(&text) {
            return Err(invalid(self.name, "string is not a registered enum value"));
        }
        Ok(())
    }
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn invalid(field: &'static str, reason: &'static str) -> DiscoveryError {
    DiscoveryError::InvalidInput { field, reason }
}
