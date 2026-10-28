// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TransformBudget, TransformError};
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// Encode a resolved workspace name like source `urllib.parse.quote(safe="")`.
///
/// UTF-8 bytes use uppercase percent escapes; spaces are not converted to `+`.
/// Caller-owned metadata resolution supplies the name, never ambient discovery.
///
/// # Errors
/// Preserves input/output/work bounds, timeout and caller cancellation.
pub fn encode_dynamic_name(
    name: &str,
    budget: &mut TransformBudget<'_>,
) -> Result<String, TransformError> {
    budget.text(name)?;
    let mut output = String::new();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            budget.push(&mut output, &char::from(byte).to_string())?;
        } else {
            let token = [
                b'%',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 15)],
            ];
            budget.push(
                &mut output,
                std::str::from_utf8(&token).expect("ASCII percent escape"),
            )?;
        }
    }
    Ok(output)
}

/// Source dynamic variable category, independent of its metadata resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicVariableKind {
    /// Current or named workspace attribute.
    Workspace,
    /// Current or named workspace item attribute.
    Item,
}

/// Validated source dynamic expression, retaining exact names and original input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicVariable {
    /// Original expression supplied to the injected metadata resolver.
    pub expression: String,
    /// Workspace or item expression.
    pub kind: DynamicVariableKind,
    /// Normalized source attribute name.
    pub attribute: String,
    /// Trimmed named workspace; None selects the current workspace.
    pub workspace_name: Option<String>,
    /// Exact supported item type for item expressions.
    pub item_type: Option<String>,
    /// Trimmed item name, preserving embedded dots.
    pub item_name: Option<String>,
}

impl DynamicVariable {
    /// Parse the actual current/legacy/named/cross-workspace source forms.
    ///
    /// # Errors
    /// Rejects missing components, unknown attributes and case-invalid item types.
    pub fn parse(expression: &str) -> Result<Self, TransformError> {
        let invalid = || {
            TransformError::Invalid(format!(
                "Invalid dynamic replacement variable: {expression}"
            ))
        };
        let mut result = Self {
            expression: expression.into(),
            kind: DynamicVariableKind::Workspace,
            attribute: "id".into(),
            workspace_name: None,
            item_type: None,
            item_name: None,
        };
        let item = if let Some(item) = expression.strip_prefix("$items.") {
            Some((item, false))
        } else if let Some(workspace) = expression.strip_prefix("$workspace.") {
            match expression {
                "$workspace.id" | "$workspace.$id" => return Ok(result),
                "$workspace.$name" => {
                    result.attribute = "name".into();
                    return Ok(result);
                }
                "$workspace.$name_encoded" => {
                    result.attribute = "name_encoded".into();
                    return Ok(result);
                }
                _ => {}
            }
            if workspace.starts_with("$items.") {
                return Err(invalid());
            }
            if let Some((name, item)) = workspace.split_once(".$items.") {
                if name.trim().is_empty() {
                    return Err(invalid());
                }
                result.workspace_name = Some(name.trim().into());
                Some((item, true))
            } else {
                if workspace.starts_with('$')
                    || (workspace.contains(".$") && !workspace.ends_with(".$id"))
                {
                    return Err(invalid());
                }
                let name = workspace.strip_suffix(".$id").unwrap_or(workspace).trim();
                if name.is_empty() {
                    return Err(invalid());
                }
                result.workspace_name = Some(name.into());
                return Ok(result);
            }
        } else {
            return Err(invalid());
        };
        let (item, cross_workspace) = item.expect("item branch");
        let (info, attribute) = if let Some(pair) = item.rsplit_once(".$") {
            pair
        } else if !cross_workspace {
            let (info, attribute) = item.rsplit_once('.').ok_or_else(invalid)?;
            (info, attribute.trim())
        } else {
            return Err(invalid());
        };
        let (item_type, name) = info.split_once('.').ok_or_else(invalid)?;
        let item_type = item_type.trim();
        let name = name.trim();
        let attribute = attribute.to_lowercase();
        if !crate::deployment_config::ITEM_TYPES.contains(&item_type)
            || name.is_empty()
            || ![
                "id",
                "sqlendpoint",
                "sqlendpointfqdn",
                "sqlendpointid",
                "queryserviceuri",
            ]
            .contains(&attribute.as_str())
        {
            return Err(invalid());
        }
        result.kind = DynamicVariableKind::Item;
        result.attribute = attribute;
        result.item_type = Some(item_type.into());
        result.item_name = Some(name.into());
        Ok(result)
    }
}

/// Item/file context supplied by the existing publication processor.
#[derive(Clone, Copy, Debug)]
pub struct ParameterItem<'a> {
    /// Exact item type.
    pub item_type: &'a str,
    /// Exact item display name.
    pub item_name: &'a str,
    /// Caller-normalized concrete file path, in the same domain as resolved filters.
    pub file_path: &'a str,
}

/// Caller-owned asynchronous metadata resolution, without a library runtime.
pub type ParameterResolution<'a> =
    Pin<Box<dyn Future<Output = Result<Value, TransformError>> + Send + 'a>>;

/// Command-local async metadata and pure inventory capabilities.
///
/// The mutable borrow allows source-conditional SDK reads without shared mutable
/// globals or a mutex. No Sync bound or unconditional prefetch/cache is imposed.
pub trait ParameterResolver: Send {
    /// Resolve a validated variable, refreshing deployed metadata as source requires.
    ///
    /// # Errors
    /// Preserve discovery, missing item/attribute, cancellation and limit failures.
    fn resolve<'a>(&'a mut self, variable: &'a DynamicVariable) -> ParameterResolution<'a>;
    /// Expand source path/glob filters to validated concrete repository file paths.
    ///
    /// The caller owns the source path syntax, repository confinement, file
    /// existence, wildcard validation and command-local cache. An empty set
    /// means no matching files, not unrestricted publication.
    ///
    /// # Errors
    /// Preserve filesystem, unsupported syntax, cancellation and limit failures.
    fn resolve_paths(&self, filters: &[String]) -> Result<Vec<String>, TransformError>;
}

/// Exact source filter conjunction over already resolved concrete paths.
pub fn matches_parameter_filters(
    item_type: Option<&Value>,
    item_name: Option<&Value>,
    paths: Option<&[String]>,
    item: ParameterItem<'_>,
) -> bool {
    fn matches(filter: Option<&Value>, value: &str) -> bool {
        match filter {
            None | Some(Value::Null) => true,
            Some(Value::String(filter)) => filter == value,
            Some(Value::Array(filters)) => {
                filters.iter().any(|filter| filter.as_str() == Some(value))
            }
            _ => false,
        }
    }
    matches(item_type, item.item_type)
        && matches(item_name, item.item_name)
        && paths.is_none_or(|paths| paths.iter().any(|path| path == item.file_path))
}

pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Filters {
    item_type: Option<Value>,
    item_name: Option<Value>,
    file_path: Option<Vec<String>>,
}

impl Filters {
    pub(super) fn parse(rule: &serde_json::Map<String, Value>) -> Result<Self, TransformError> {
        fn validate(value: &Value) -> bool {
            !truthy(value)
                || value.is_string()
                || value
                    .as_array()
                    .is_some_and(|values| values.iter().all(Value::is_string))
        }
        for key in ["item_type", "item_name", "file_path"] {
            if rule.get(key).is_some_and(|value| !validate(value)) {
                return Err(TransformError::Invalid(format!(
                    "{key} must be a string or list of strings"
                )));
            }
        }
        let file_path = rule
            .get("file_path")
            .filter(|value| truthy(value))
            .map(|value| {
                value.as_array().map_or_else(
                    || vec![value.as_str().expect("validated filter").into()],
                    |values| {
                        values
                            .iter()
                            .map(|value| value.as_str().expect("validated filter").into())
                            .collect()
                    },
                )
            });
        Ok(Self {
            item_type: rule.get("item_type").cloned(),
            item_name: rule.get("item_name").cloned(),
            file_path,
        })
    }

    pub(super) fn requires_context(&self) -> bool {
        self.item_type
            .as_ref()
            .is_some_and(|value| !value.is_null())
            || self
                .item_name
                .as_ref()
                .is_some_and(|value| !value.is_null())
            || self.file_path.is_some()
    }

    pub(super) fn matches(
        &self,
        item: ParameterItem<'_>,
        resolver: &dyn ParameterResolver,
        budget: &mut TransformBudget<'_>,
    ) -> Result<bool, TransformError> {
        budget.checkpoint()?;
        let paths = self
            .file_path
            .as_ref()
            .map(|filters| resolver.resolve_paths(filters))
            .transpose()?;
        if let Some(paths) = &paths {
            for path in paths {
                budget.text(path)?;
            }
        }
        Ok(matches_parameter_filters(
            self.item_type.as_ref(),
            self.item_name.as_ref(),
            paths.as_deref(),
            item,
        ))
    }
}

pub(super) struct NoResolver;
impl ParameterResolver for NoResolver {
    fn resolve<'a>(&'a mut self, _: &'a DynamicVariable) -> ParameterResolution<'a> {
        Box::pin(std::future::ready(Err(TransformError::Unsupported(
            "dynamic variable requires caller metadata resolver",
        ))))
    }
    fn resolve_paths(&self, _: &[String]) -> Result<Vec<String>, TransformError> {
        Err(TransformError::Unsupported(
            "file filters require caller repository resolver",
        ))
    }
}
