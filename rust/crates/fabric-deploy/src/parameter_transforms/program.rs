// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    DynamicVariable, FindReplacement, JsonPathReplacement, ParameterItem, ParameterResolver,
    TransformBudget, TransformError, TransformLimits,
    context::{Filters, NoResolver},
};
use crate::{config::process_environment_key, parameters::ParameterNotice};
use serde_json::{Map, Value};
use std::fmt;

#[derive(Clone)]
struct FindRule {
    query: Option<FindReplacement>,
    dynamic_find: Option<DynamicVariable>,
    ignore_case: bool,
    replacement: Option<String>,
    filters: Filters,
}

#[derive(Clone)]
struct KeyRule {
    query: JsonPathReplacement,
    replacement: Option<Value>,
    filters: Filters,
}

/// A source parameter rule skipped for an absent exact target environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkippedParameter {
    /// Parameter section name.
    pub parameter: String,
    /// Zero-based source rule index.
    pub index: usize,
    /// Exact requested environment.
    pub environment: String,
}

/// Validated command-local parameter program over the same bounded primitives.
///
/// Full schema uses source string flags; leaf fixture booleans are not accepted
/// as configuration. Filters, templates and dynamic metadata use explicit caller
/// capabilities; unsupported syntax never becomes unchanged-data success.
#[derive(Clone)]
pub struct ParameterProgram {
    document: Value,
    environment: String,
    key_rules: Vec<KeyRule>,
    find_rules: Vec<FindRule>,
    notices: Vec<ParameterNotice>,
    skipped: Vec<SkippedParameter>,
    item_settings: Map<String, Value>,
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
        if values.len() != 1 && environment != "N/A" {
            return Err(invalid(
                "The reserved _ALL_ environment key must be the sole replacement key",
            ));
        }
        if values.len() == 1 {
            let replacement = values[&key].as_str().map_or_else(
                || fabric_core_json::json_dumps(&values[&key]),
                str::to_owned,
            );
            notices.push(ParameterNotice::AllEnvironment { key, replacement });
        }
    }
    process_environment_key(environment, &mut values);
    Ok(values)
}

fn flag(rule: &Map<String, Value>, key: &str) -> Result<bool, TransformError> {
    match rule.get(key).filter(|value| !value.is_null()) {
        None => Ok(false),
        Some(Value::String(value)) => Ok(value.eq_ignore_ascii_case("true")),
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
        Self::from_document_with_features(document, environment, false, budget)
    }

    /// Validate parameters using the frozen command's environment-variable feature.
    ///
    /// Raw environment expansion occurs before YAML decoding via
    /// [`super::expand_parameter_environment`]. Unresolved active tokens fail.
    ///
    /// # Errors
    /// Retains all schema, syntax, dynamic-variable and bounded-work failures.
    pub fn from_document_with_features(
        document: &Value,
        environment: &str,
        environment_variable_replacement: bool,
        budget: &mut TransformBudget<'_>,
    ) -> Result<Self, TransformError> {
        budget.checkpoint()?;
        super::jsonpath::validate_value(document, budget, 0)?;
        let document = document
            .as_object()
            .ok_or_else(|| invalid("Parameter file must be a dictionary"))?;
        if document.contains_key("extend") {
            return Err(TransformError::Unsupported(
                "prepare templates through caller ParameterFileHost before compilation",
            ));
        }
        let item_settings = super::schema::item_settings(document, environment, budget)?;
        if document.keys().any(|key| {
            ![
                "find_replace",
                "key_value_replace",
                "spark_pool",
                "gateway_binding",
                "semantic_model_binding",
                "spark_pool_replace_value",
            ]
            .contains(&key.as_str())
        }) {
            return Err(invalid("Invalid parameter name"));
        }
        if !document.keys().any(|key| {
            [
                "find_replace",
                "key_value_replace",
                "spark_pool",
                "gateway_binding",
                "semantic_model_binding",
            ]
            .contains(&key.as_str())
        }) {
            return Err(invalid("Parameter file has no parameter sections"));
        }
        let mut notices = Vec::new();
        let mut find_rules = Vec::new();
        let mut key_rules = Vec::new();
        let mut skipped = Vec::new();
        if let Some(rules) = document.get("key_value_replace") {
            for (index, rule) in rules
                .as_array()
                .ok_or_else(|| invalid("key_value_replace must be a list"))?
                .iter()
                .enumerate()
            {
                budget.checkpoint()?;
                let rule = rule
                    .as_object()
                    .ok_or_else(|| invalid("Parameter rule must be a dictionary"))?;
                if rule.keys().any(|key| {
                    ![
                        "find_key",
                        "replace_value",
                        "item_type",
                        "item_name",
                        "file_path",
                    ]
                    .contains(&key.as_str())
                }) {
                    return Err(invalid("key_value_replace contains invalid keys"));
                }
                let expression = rule
                    .get("find_key")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| invalid("Missing or empty find_key"))?;
                let query = JsonPathReplacement::compile(expression, budget)?;
                let values = values(rule, environment, &mut notices)?;
                validate_variables(&values, environment, environment_variable_replacement)?;
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
                    filters: Filters::parse(rule)?,
                });
                if !values.contains_key(environment) {
                    skipped.push(SkippedParameter {
                        parameter: "key_value_replace".into(),
                        index,
                        environment: environment.into(),
                    });
                }
            }
        }
        let key_notices = std::mem::take(&mut notices);
        if let Some(rules) = document.get("find_replace") {
            for (index, rule) in rules
                .as_array()
                .ok_or_else(|| invalid("find_replace must be a list"))?
                .iter()
                .enumerate()
            {
                budget.checkpoint()?;
                let rule = rule
                    .as_object()
                    .ok_or_else(|| invalid("Parameter rule must be a dictionary"))?;
                if rule.keys().any(|key| {
                    ![
                        "find_value",
                        "replace_value",
                        "is_regex",
                        "ignore_case",
                        "item_type",
                        "item_name",
                        "file_path",
                    ]
                    .contains(&key.as_str())
                }) {
                    return Err(invalid("find_replace contains invalid keys"));
                }
                let find = rule
                    .get("find_value")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid("Missing or empty find_value"))?;
                let is_regex = flag(rule, "is_regex")?;
                let ignore_case = flag(rule, "ignore_case")?;
                let dynamic_find = if find.starts_with('$') {
                    if find.starts_with("$items.") || (is_regex && find.starts_with("$workspace."))
                    {
                        return Err(invalid(
                            "Dynamic find_value is incompatible with items variables or regex",
                        ));
                    }
                    Some(DynamicVariable::parse(find)?)
                } else {
                    None
                };
                let query = if dynamic_find.is_none() {
                    Some(FindReplacement::compile(
                        find,
                        is_regex,
                        ignore_case,
                        budget,
                    )?)
                } else {
                    None
                };
                if !is_regex
                    && rule
                        .get("is_regex")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty())
                {
                    notices.push(ParameterNotice::RegexIgnored);
                }
                let values = values(rule, environment, &mut notices)?;
                validate_variables(&values, environment, environment_variable_replacement)?;
                if values
                    .values()
                    .any(|value| value.as_str().is_none_or(|value| value.is_empty()))
                {
                    return Err(invalid(
                        "Every find_replace environment replacement must be a nonempty string",
                    ));
                }
                let replacement = values
                    .get(environment)
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if replacement.is_none() {
                    skipped.push(SkippedParameter {
                        parameter: "find_replace".into(),
                        index,
                        environment: environment.into(),
                    });
                }
                find_rules.push(FindRule {
                    query,
                    dynamic_find,
                    ignore_case,
                    replacement,
                    filters: Filters::parse(rule)?,
                });
            }
        }
        notices.extend(key_notices);
        Ok(Self {
            document: Value::Object(document.clone()),
            environment: environment.into(),
            key_rules,
            find_rules,
            notices,
            skipped,
            item_settings,
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
        if self
            .key_rules
            .iter()
            .any(|rule| rule.filters.requires_context())
            || self
                .find_rules
                .iter()
                .any(|rule| rule.filters.requires_context())
        {
            return Err(TransformError::Unsupported(
                "parameter filters require caller item context",
            ));
        }
        if !self.item_settings.is_empty() {
            return Err(TransformError::Unsupported(
                "item-specific parameters require caller processor",
            ));
        }
        self.apply_for_item(
            content,
            ParameterItem {
                item_type: "",
                item_name: "",
                file_path: file_name,
            },
            &NoResolver,
            budget,
        )
    }

    /// Apply source-ordered rules with caller-injected item, metadata and path context.
    ///
    /// Item-specific settings are deliberately exposed separately for their
    /// processor and never interpreted as text replacement rules.
    ///
    /// # Errors
    /// Preserves filter resolution, dynamic metadata and bounded transform failures.
    pub fn apply_for_item(
        &self,
        content: &str,
        item: ParameterItem<'_>,
        resolver: &dyn ParameterResolver,
        budget: &mut TransformBudget<'_>,
    ) -> Result<String, TransformError> {
        budget.text(content)?;
        let mut content = content.to_owned();
        for rule in &self.key_rules {
            budget.checkpoint()?;
            if !rule.filters.matches(item, resolver, budget)? {
                continue;
            }
            let json = serde_json::from_str::<Value>(&content).is_ok();
            let name = item.file_path.to_lowercase();
            if json || name.ends_with(".yml") || name.ends_with(".yaml") {
                if !json {
                    match crate::yaml::decode(
                        &content,
                        crate::yaml::YamlPolicy::Config,
                        crate::yaml::YamlLimits {
                            max_bytes: budget.limits.max_bytes,
                            max_nodes: budget.limits.max_visits,
                            max_depth: budget.limits.max_depth,
                        },
                    ) {
                        Ok(_) => {}
                        Err(crate::deployment_config::ConfigurationError::Invalid(_)) => continue,
                        Err(crate::deployment_config::ConfigurationError::Limit) => {
                            return Err(TransformError::Limit);
                        }
                        Err(crate::deployment_config::ConfigurationError::Transform(error)) => {
                            return Err(*error);
                        }
                        Err(crate::deployment_config::ConfigurationError::Unsupported(_)) => {
                            return Err(TransformError::Unsupported(
                                "YAML value outside the parameter JSON domain",
                            ));
                        }
                    }
                }
                content = rule.query.apply_resolving(
                    &content,
                    rule.replacement.as_ref(),
                    !json,
                    |value, budget| resolve(value, resolver, budget),
                    budget,
                )?;
            }
        }
        for rule in &self.find_rules {
            let filter_match = rule.filters.matches(item, resolver, budget)?;
            let resolved_find;
            let dynamic_query;
            let query = if let Some(variable) = &rule.dynamic_find {
                resolved_find = resolver.resolve_dynamic(variable)?;
                budget.text(&resolved_find)?;
                if resolved_find.is_empty() {
                    continue;
                }
                dynamic_query =
                    FindReplacement::compile(&resolved_find, false, rule.ignore_case, budget)?;
                &dynamic_query
            } else {
                rule.query.as_ref().expect("compiled query")
            };
            if !query.has_matches(&content, filter_match, budget)? {
                continue;
            }
            if let Some(replacement) = &rule.replacement {
                let replacement = resolve(&Value::String(replacement.clone()), resolver, budget)?;
                content = query.apply(
                    &content,
                    replacement.as_str().expect("resolved string"),
                    filter_match,
                    budget,
                )?;
            }
        }
        Ok(content)
    }

    /// Apply scoped source rules through the deployment engine's caller context.
    ///
    /// Alias of [`Self::apply_for_item`]; item processors retain ownership of
    /// item-specific settings, SDK metadata lookup and repository discovery.
    ///
    /// # Errors
    /// Preserves the same syntax, resolver, schema and bounded-work failures.
    pub fn apply_scoped(
        &self,
        content: &str,
        item: ParameterItem<'_>,
        resolver: &dyn ParameterResolver,
        budget: &mut TransformBudget<'_>,
    ) -> Result<String, TransformError> {
        self.apply_for_item(content, item, resolver, budget)
    }

    /// Ordered source notice outcomes; caller owns delivery and failures.
    pub fn notices(&self) -> &[ParameterNotice] {
        &self.notices
    }

    /// Ordered rule skip diagnostics for caller presentation.
    pub fn skipped(&self) -> &[SkippedParameter] {
        &self.skipped
    }

    /// Validated source item-specific schemas retained for the owning processors.
    pub fn item_settings(&self) -> &Map<String, Value> {
        &self.item_settings
    }

    /// Full prepared source document for dependency planning and typed processors.
    pub fn document(&self) -> &Value {
        &self.document
    }

    /// Validated prepared source schema, including raw rules and item settings.
    ///
    /// Alias of [`Self::document`] for deployment-engine consumers. Replacement
    /// mappings, source string flags, filters and dynamic expressions remain
    /// lossless and are not rewritten during item application.
    pub fn source_document(&self) -> &Value {
        self.document()
    }

    /// Exact selected target environment used by this immutable program.
    pub fn environment(&self) -> &str {
        &self.environment
    }

    /// Check that the owning processors explicitly consume item-specific settings.
    ///
    /// # Errors
    /// Names the first unconsumed parameter section before publication.
    pub fn require_item_settings_consumed(&self, sections: &[&str]) -> Result<(), TransformError> {
        if let Some(key) = self.item_settings.keys().find(|key| {
            !sections.contains(&key.as_str())
                && !(key.as_str() == "gateway_binding"
                    && self.item_settings.contains_key("semantic_model_binding")
                    && sections.contains(&"semantic_model_binding"))
        }) {
            return Err(TransformError::Invalid(format!(
                "Unconsumed item-specific parameter section: {key}"
            )));
        }
        Ok(())
    }
}

fn validate_variables(
    values: &Map<String, Value>,
    environment: &str,
    enabled: bool,
) -> Result<(), TransformError> {
    for (key, value) in values {
        if let Some(value) = value.as_str().filter(|value| value.starts_with('$')) {
            if value.starts_with("$ENV:") {
                if !enabled || key == environment || key.to_lowercase() == "_all_" {
                    return Err(invalid(
                        "Unresolved or disabled parameter environment variable",
                    ));
                }
            } else {
                DynamicVariable::parse(value)?;
            }
        }
    }
    Ok(())
}

fn resolve(
    value: &Value,
    resolver: &dyn ParameterResolver,
    budget: &mut TransformBudget<'_>,
) -> Result<Value, TransformError> {
    if let Some(value) = value
        .as_str()
        .filter(|value| value.starts_with('$') && !value.starts_with("$ENV:"))
    {
        let resolved = resolver.resolve_dynamic(&DynamicVariable::parse(value)?)?;
        budget.text(&resolved)?;
        return Ok(Value::String(resolved));
    }
    Ok(value.clone())
}
