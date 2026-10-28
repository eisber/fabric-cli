// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    FindReplacement, JsonPathReplacement, TransformBudget, TransformError, TransformLimits,
};
use crate::{config::process_environment_key, parameters::ParameterNotice};
use serde_json::{Map, Value};
use std::fmt;

#[derive(Clone)]
struct FindRule {
    query: FindReplacement,
    replacement: String,
}

#[derive(Clone)]
struct KeyRule {
    query: JsonPathReplacement,
    replacement: Option<Value>,
}

/// Validated command-local parameter program over the same bounded primitives.
///
/// Full schema uses source string flags; leaf fixture booleans are not accepted
/// as configuration. Unsupported filters/templates/dynamics never disappear.
#[derive(Clone)]
pub struct ParameterProgram {
    key_rules: Vec<KeyRule>,
    find_rules: Vec<FindRule>,
    notices: Vec<ParameterNotice>,
}

impl fmt::Debug for ParameterProgram {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParameterProgram")
            .field("key_rule_count", &self.key_rules.len())
            .field("find_rule_count", &self.find_rules.len())
            .finish_non_exhaustive()
    }
}

fn invalid(message: &str) -> TransformError {
    TransformError::Invalid(message.into())
}

fn values(
    rule: &Map<String, Value>,
    environment: &str,
    notices: &mut Vec<ParameterNotice>,
) -> Result<Map<String, Value>, TransformError> {
    let mut values = rule
        .get("replace_value")
        .and_then(Value::as_object)
        .cloned()
        .filter(|mapping| !mapping.is_empty())
        .ok_or_else(|| invalid("replace_value must be a nonempty dictionary"))?;
    if let Some(key) = values
        .keys()
        .find(|key| key.to_lowercase() == "_all_")
        .cloned()
    {
        if values.len() != 1 {
            return Err(invalid(
                "The reserved _ALL_ environment key must be the sole replacement key",
            ));
        }
        let replacement = values[&key]
            .as_str()
            .ok_or(TransformError::Unsupported(
                "non-string _ALL_ parameter notice",
            ))?
            .to_owned();
        notices.push(ParameterNotice::AllEnvironment { key, replacement });
    }
    if values
        .values()
        .any(|value| value.as_str().is_some_and(|text| text.starts_with('$')))
    {
        return Err(TransformError::Unsupported(
            "dynamic parameter replacement variables",
        ));
    }
    process_environment_key(environment, &mut values);
    Ok(values)
}

fn flag(rule: &Map<String, Value>, key: &str) -> Result<bool, TransformError> {
    match rule.get(key).filter(|value| !value.is_null()) {
        None => Ok(false),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("true") => Ok(true),
        Some(Value::String(value)) if value.is_empty() || value.eq_ignore_ascii_case("false") => {
            Ok(false)
        }
        Some(Value::String(_)) => Err(TransformError::Unsupported(
            "noncanonical parameter flag warnings",
        )),
        _ => Err(invalid(
            "Parameter flags must be strings, not YAML booleans",
        )),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

impl ParameterProgram {
    /// Validate every source rule before processing any item or service mutation.
    ///
    /// # Errors
    /// Retains malformed/unsupported schema, pattern, typing and resource errors.
    pub fn from_document(
        document: &Value,
        environment: &str,
        budget: &mut TransformBudget<'_>,
    ) -> Result<Self, TransformError> {
        budget.checkpoint()?;
        super::jsonpath::validate_value(document, budget, 0)?;
        let document = document
            .as_object()
            .ok_or_else(|| invalid("Parameter file must be a dictionary"))?;
        if document
            .keys()
            .any(|key| !["find_replace", "key_value_replace"].contains(&key.as_str()))
        {
            return Err(TransformError::Unsupported(
                "parameter templates, filters or item-specific schema",
            ));
        }
        if document.is_empty() {
            return Err(invalid("Parameter file has no parameter sections"));
        }
        let mut notices = Vec::new();
        let mut find_rules = Vec::new();
        let mut key_rules = Vec::new();
        if let Some(rules) = document.get("key_value_replace") {
            for rule in rules
                .as_array()
                .ok_or_else(|| invalid("key_value_replace must be a list"))?
            {
                budget.checkpoint()?;
                let rule = rule
                    .as_object()
                    .ok_or_else(|| invalid("Parameter rule must be a dictionary"))?;
                if rule
                    .keys()
                    .any(|key| !["find_key", "replace_value"].contains(&key.as_str()))
                {
                    return Err(TransformError::Unsupported("key_value_replace filters"));
                }
                let expression = rule
                    .get("find_key")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| invalid("Missing or empty find_key"))?;
                let query = JsonPathReplacement::compile(expression, budget)?;
                let values = values(rule, environment, &mut notices)?;
                let expected = type_name(values.values().next().expect("nonempty mapping"));
                if values
                    .values()
                    .any(|value| value.is_null() || type_name(value) != expected)
                {
                    return Err(invalid(
                        "key_value_replace environment values must be nonnull and have exactly the same type",
                    ));
                }
                key_rules.push(KeyRule {
                    query,
                    replacement: values.get(environment).cloned(),
                });
            }
        }
        let key_notices = std::mem::take(&mut notices);
        if let Some(rules) = document.get("find_replace") {
            for rule in rules
                .as_array()
                .ok_or_else(|| invalid("find_replace must be a list"))?
            {
                budget.checkpoint()?;
                let rule = rule
                    .as_object()
                    .ok_or_else(|| invalid("Parameter rule must be a dictionary"))?;
                if rule.keys().any(|key| {
                    !["find_value", "replace_value", "is_regex", "ignore_case"]
                        .contains(&key.as_str())
                }) {
                    return Err(TransformError::Unsupported("find_replace filters"));
                }
                let find = rule
                    .get("find_value")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid("Missing or empty find_value"))?;
                if find.starts_with('$') {
                    return Err(TransformError::Unsupported("dynamic find_value variables"));
                }
                let is_regex = flag(rule, "is_regex")?;
                let ignore_case = flag(rule, "ignore_case")?;
                let query = FindReplacement::compile(find, is_regex, ignore_case, budget)?;
                if !is_regex
                    && rule
                        .get("is_regex")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty())
                {
                    notices.push(ParameterNotice::RegexIgnored);
                }
                let values = values(rule, environment, &mut notices)?;
                if values
                    .values()
                    .any(|value| value.as_str().is_none_or(|value| value.is_empty()))
                {
                    return Err(invalid(
                        "Every find_replace environment replacement must be a nonempty string",
                    ));
                }
                let replacement = values.get(environment).and_then(Value::as_str).ok_or(
                    TransformError::Unsupported(
                        "missing find_replace target environment skip notice",
                    ),
                )?;
                find_rules.push(FindRule {
                    query,
                    replacement: replacement.into(),
                });
            }
        }
        notices.extend(key_notices);
        Ok(Self {
            key_rules,
            find_rules,
            notices,
        })
    }

    /// Compile with explicit default finite limits and no cancellation signal.
    ///
    /// # Errors
    /// Same errors as [`Self::from_document`]; callers needing cancellation share
    /// their own budget with compilation and subsequent application.
    pub fn with_default_limits(
        document: &Value,
        environment: &str,
    ) -> Result<Self, TransformError> {
        Self::from_document(
            document,
            environment,
            &mut TransformBudget::new(TransformLimits::default(), &|| false),
        )
    }

    /// Apply key/value rules before find rules, independent of mapping insertion.
    ///
    /// `.platform` and nontext files must be excluded by the existing item codec.
    ///
    /// # Errors
    /// Propagates original typed input, compatibility and bounded-work failures.
    pub fn apply(
        &self,
        content: &str,
        file_name: &str,
        budget: &mut TransformBudget<'_>,
    ) -> Result<String, TransformError> {
        budget.text(content)?;
        let mut content = content.to_owned();
        for rule in &self.key_rules {
            budget.checkpoint()?;
            let json = serde_json::from_str::<Value>(&content).is_ok();
            let name = file_name.to_lowercase();
            if json || name.ends_with(".yml") || name.ends_with(".yaml") {
                content = rule
                    .query
                    .apply(&content, rule.replacement.as_ref(), !json, budget)?;
            }
        }
        for rule in &self.find_rules {
            content = rule
                .query
                .apply(&content, &rule.replacement, true, budget)?;
        }
        Ok(content)
    }

    /// Ordered source notice outcomes; caller owns delivery and failures.
    pub fn notices(&self) -> &[ParameterNotice] {
        &self.notices
    }
}
