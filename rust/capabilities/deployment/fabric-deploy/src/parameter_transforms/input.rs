// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TransformBudget, TransformError};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Caller-owned template loading, resolved relative to the main parameter file.
pub trait ParameterFileHost {
    /// Read/decode a parameter template with duplicate-key and environment policy.
    ///
    /// None means missing/invalid/empty input, which source logs and skips.
    ///
    /// # Errors
    /// Preserve hard cancellation/resource errors; ordinary invalid templates
    /// should return None so the source skip-and-continue behavior is retained.
    fn template(&self, reference: &str) -> Result<Option<Value>, TransformError>;
}

/// Prepared parameter document plus source-visible template outcomes.
#[derive(Clone, Debug)]
pub struct PreparedParameters {
    /// Base rules followed by successfully loaded template rules in source order.
    pub document: Value,
    /// Template references skipped because they were duplicate, missing or nested.
    pub skipped_templates: Vec<String>,
}

/// Expand exact `$ENV:` tokens using an injected snapshot, before YAML decoding.
///
/// Unset values keep the token; empty defined values become empty strings.
/// Feature matching belongs to the command configuration, not this primitive.
///
/// # Errors
/// Returns original bounded-work, timeout or cancellation failures.
pub fn expand_parameter_environment(
    raw: &str,
    enabled: bool,
    environment: &BTreeMap<String, String>,
    budget: &mut TransformBudget<'_>,
) -> Result<String, TransformError> {
    budget.text(raw)?;
    if !enabled {
        return Ok(raw.into());
    }
    let mut output = String::new();
    let mut remainder = raw;
    while let Some(index) = remainder.find("$ENV:") {
        budget.push(&mut output, &remainder[..index])?;
        remainder = &remainder[index + 5..];
        let length = remainder
            .char_indices()
            .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
            .map(|(index, character)| index + character.len_utf8())
            .last()
            .unwrap_or(0);
        let variable = &remainder[..length];
        if let Some(value) = environment.get(variable).filter(|_| length != 0) {
            budget.push(&mut output, value)?;
        } else {
            budget.push(&mut output, "$ENV:")?;
            budget.push(&mut output, variable)?;
        }
        remainder = &remainder[length..];
    }
    budget.push(&mut output, remainder)?;
    Ok(output)
}

/// Source recursive template merge: append lists, recurse maps, retain mismatches.
///
/// # Errors
/// Rejects non-dictionary roots and bounded allocation/depth/work exhaustion.
pub fn merge_parameter_template(
    base: &Value,
    template: &Value,
    budget: &mut TransformBudget<'_>,
) -> Result<Value, TransformError> {
    fn merge(
        base: &Map<String, Value>,
        template: &Map<String, Value>,
        budget: &mut TransformBudget<'_>,
        depth: usize,
    ) -> Result<Map<String, Value>, TransformError> {
        budget.depth(depth)?;
        let mut result = base.clone();
        for (key, value) in template {
            budget.checkpoint()?;
            if key == "extend" {
                continue;
            }
            let value = match result.get(key) {
                None => value.clone(),
                Some(Value::Array(base)) if value.is_array() => {
                    let mut values = base.clone();
                    values.extend(value.as_array().expect("array").iter().cloned());
                    Value::Array(values)
                }
                Some(Value::Object(base)) if value.is_object() => Value::Object(merge(
                    base,
                    value.as_object().expect("object"),
                    budget,
                    depth + 1,
                )?),
                Some(base) => Value::Array(vec![base.clone(), value.clone()]),
            };
            result.insert(key.clone(), value);
        }
        Ok(result)
    }
    super::jsonpath::validate_value(base, budget, 0)?;
    super::jsonpath::validate_value(template, budget, 0)?;
    let invalid = || TransformError::Invalid("Parameter template must be a dictionary".into());
    let result = Value::Object(merge(
        base.as_object().ok_or_else(invalid)?,
        template.as_object().ok_or_else(invalid)?,
        budget,
        0,
    )?);
    super::jsonpath::validate_value(&result, budget, 0)?;
    budget.text(&fabric_core_json::json_dumps(&result))?;
    Ok(result)
}

/// Load templates through the caller, preserving source append/dedup/skip order.
///
/// # Errors
/// Rejects invalid base roots and propagates hard host/resource/cancellation errors.
pub fn prepare_parameter_document(
    document: &Value,
    host: &dyn ParameterFileHost,
    budget: &mut TransformBudget<'_>,
) -> Result<PreparedParameters, TransformError> {
    super::jsonpath::validate_value(document, budget, 0)?;
    let mut document = document
        .as_object()
        .cloned()
        .ok_or_else(|| TransformError::Invalid("Parameter file must be a dictionary".into()))?;
    let templates = document.remove("extend");
    let mut output = PreparedParameters {
        document: Value::Object(document),
        skipped_templates: Vec::new(),
    };
    let Some(templates) = templates else {
        return Ok(output);
    };
    if !super::context::truthy(&templates) {
        return Err(TransformError::Invalid(
            "Source leaves a falsey extend key unprocessed, which is an invalid parameter name"
                .into(),
        ));
    }
    let Some(templates) = templates.as_array() else {
        output.skipped_templates.push("extend is not a list".into());
        return Ok(output);
    };
    let mut processed: Vec<Value> = Vec::new();
    for template in templates {
        budget.checkpoint()?;
        let reference = match template {
            Value::String(value) => value.clone(),
            Value::Null => "None".into(),
            Value::Bool(value) => if *value { "True" } else { "False" }.into(),
            Value::Number(_) => fabric_core_json::json_dumps(template),
            _ => {
                output
                    .skipped_templates
                    .push("unhashable template reference".into());
                continue;
            }
        };
        if processed.contains(template) {
            output.skipped_templates.push(reference);
            continue;
        }
        let Some(value) = host.template(&reference)? else {
            output.skipped_templates.push(reference);
            continue;
        };
        if value.as_object().is_some_and(Map::is_empty) {
            continue;
        }
        if value.get("extend").is_some() || !value.is_object() {
            output.skipped_templates.push(reference);
            continue;
        }
        output.document = merge_parameter_template(&output.document, &value, budget)?;
        processed.push(template.clone());
    }
    Ok(output)
}
