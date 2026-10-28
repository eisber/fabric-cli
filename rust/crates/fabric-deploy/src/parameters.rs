// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded source parameter schema for ordered case-sensitive literal rules.

use crate::{config::process_environment_key, deployment_config::ConfigurationError};
use serde_json::Value;
use std::fmt;

/// Validated source parameter rules, not an implicit repository-file discovery.
#[derive(Clone)]
pub struct LiteralParameters {
    literals: Vec<(String, String)>,
    notices: Vec<ParameterNotice>,
}

impl fmt::Debug for LiteralParameters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiteralParameters")
            .field("rule_count", &self.literals.len())
            .field("notice_count", &self.notices.len())
            .finish()
    }
}

/// Source-visible parameter notices presented by the caller, never global logging.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParameterNotice {
    /// An explicitly false regex string requests the source ignored-regex warning.
    RegexIgnored,
    /// A sole reserved alias applies its replacement to the requested environment.
    AllEnvironment {
        /// Original case-preserved alias key.
        key: String,
        /// Source-selected replacement value.
        replacement: String,
    },
}

fn invalid(message: &str) -> ConfigurationError {
    ConfigurationError::Invalid(vec![message.into()])
}

fn unsupported(branch: &str) -> ConfigurationError {
    ConfigurationError::Unsupported(branch.into())
}

impl LiteralParameters {
    /// Validate the literal-only source schema and resolve its exact environment.
    ///
    /// `is_regex` and `ignore_case` use source string flags, not YAML booleans.
    /// Every rule/environment is checked before target or service selection.
    ///
    /// # Errors
    /// Rejects invalid schema/types and explicitly refuses unported templates,
    /// regex, JSONPath, filters, dynamic variables and missing-environment notices.
    pub fn from_document(document: &Value, environment: &str) -> Result<Self, ConfigurationError> {
        let document = document
            .as_object()
            .ok_or_else(|| invalid("Parameter file must be a dictionary"))?;
        if let Some(key) = document.keys().find(|key| key.as_str() != "find_replace") {
            return Err(unsupported(&format!("parameter.{key}")));
        }
        let rules = document
            .get("find_replace")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("find_replace parameter must be a list"))?;
        let mut literals = Vec::new();
        let mut notices = Vec::new();
        for rule in rules {
            let rule = rule
                .as_object()
                .ok_or_else(|| invalid("find_replace rule must be a dictionary"))?;
            if let Some(key) = rule.keys().find(|key| {
                !["find_value", "replace_value", "is_regex", "ignore_case"].contains(&key.as_str())
            }) {
                return Err(unsupported(&format!("parameter.find_replace.{key}")));
            }
            let find = rule
                .get("find_value")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("find_replace requires a nonempty string find_value"))?;
            if find.starts_with('$') {
                return Err(unsupported("dynamic find_value variables"));
            }
            for flag in ["is_regex", "ignore_case"] {
                if let Some(value) = rule.get(flag).filter(|value| !value.is_null()) {
                    let value = value.as_str().ok_or_else(|| {
                        invalid("Parameter flags must be strings, not YAML booleans")
                    })?;
                    if value.eq_ignore_ascii_case("true") {
                        return Err(unsupported(&format!("parameter.find_replace.{flag}")));
                    }
                    if flag == "is_regex" && !value.is_empty() {
                        notices.push(ParameterNotice::RegexIgnored);
                    }
                }
            }
            let mut values = rule
                .get("replace_value")
                .and_then(Value::as_object)
                .cloned()
                .filter(|mapping| !mapping.is_empty())
                .ok_or_else(|| {
                    invalid("find_replace requires a nonempty replace_value dictionary")
                })?;
            if values
                .values()
                .any(|value| value.as_str().is_none_or(|value| value.is_empty()))
            {
                return Err(invalid(
                    "Every find_replace environment replacement must be a nonempty string",
                ));
            }
            if values
                .values()
                .any(|value| value.as_str().is_some_and(|value| value.starts_with('$')))
            {
                return Err(unsupported("dynamic replace_value variables"));
            }
            let alias = values
                .keys()
                .find(|key| key.to_lowercase() == "_all_")
                .cloned();
            if let Some(key) = alias {
                if values.len() != 1 {
                    return Err(invalid(
                        "The reserved _ALL_ environment key must be the sole replacement key",
                    ));
                }
                notices.push(ParameterNotice::AllEnvironment {
                    replacement: values[&key].as_str().expect("validated replacement").into(),
                    key,
                });
            }
            process_environment_key(environment, &mut values);
            let replacement = values
                .get(environment)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    unsupported(
                        "parameter rule with missing target environment and source skip notice",
                    )
                })?;
            literals.push((find.into(), replacement.into()));
        }
        Ok(Self { literals, notices })
    }

    /// Ordered literal pairs, applied after logical IDs and before workspace IDs.
    pub fn literals(&self) -> &[(String, String)] {
        &self.literals
    }

    /// Source warning outcomes; caller owns formatting, delivery and failures.
    pub fn notices(&self) -> &[ParameterNotice] {
        &self.notices
    }
}
