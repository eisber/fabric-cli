// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parsed-JSON equivalents of pinned CICD environment-selection helpers.

use serde_json::{Map, Value};
use std::fmt;

/// Invalid structural input to a configuration helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigError;

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("configuration requires an object and a core object for workspace settings")
    }
}

impl std::error::Error for ConfigError {}

/// Selects an exact environment key, or a non-mapped single value.
///
/// JSON null and missing values both correspond to Python `None`.
/// # Errors
/// Rejects a non-object section rather than substituting empty settings.
pub fn get_config_value<'a>(
    section: &'a Value,
    key: &str,
    environment: &str,
) -> Result<Option<&'a Value>, ConfigError> {
    let section = section.as_object().ok_or(ConfigError)?;
    let value = section.get(key).and_then(|value| {
        if let Value::Object(mapping) = value {
            mapping.get(environment)
        } else {
            Some(value)
        }
    });
    Ok(value.filter(|value| !value.is_null()))
}

/// Extracts workspace settings without resolving paths or validating a deploy file.
///
/// Preserves workspace-ID precedence, exact case, environment trimming, and
/// omission of absent optional settings from the pinned Python helper.
/// # Errors
/// Rejects missing/non-object `core`; never treats malformed input as success.
pub fn extract_workspace_settings(
    config: &Value,
    environment: &str,
) -> Result<Map<String, Value>, ConfigError> {
    let core = config
        .get("core")
        .filter(|value| value.is_object())
        .ok_or(ConfigError)?;
    let environment = environment.trim();
    let mut settings = Map::new();
    let workspace_key = if core.get("workspace_id").is_some() {
        Some(("workspace_id", "workspace_id"))
    } else if core.get("workspace").is_some() {
        Some(("workspace", "workspace_name"))
    } else {
        None
    };
    if let Some((input, output)) = workspace_key {
        let value = get_config_value(core, input, environment)?
            .cloned()
            .unwrap_or(Value::Null);
        settings.insert(output.to_owned(), value);
    }
    if core.get("repository_directory").is_some() {
        settings.insert(
            "repository_directory".to_owned(),
            get_config_value(core, "repository_directory", environment)?
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    for (input, output) in [
        ("item_types_in_scope", "item_types_in_scope"),
        ("parameter", "parameter_file_path"),
    ] {
        if let Some(value) = get_config_value(core, input, environment)? {
            settings.insert(output.to_owned(), value.clone());
        }
    }
    Ok(settings)
}

/// Mutates the sole case-insensitive `_ALL_` alias to the exact target key.
///
/// Mixed alias/specific mappings remain unchanged at this leaf boundary; this
/// is not full parameter validation or permission to accept mixed input.
pub fn process_environment_key(environment: &str, mapping: &mut Map<String, Value>) {
    if mapping.len() == 1 {
        let alias = mapping
            .keys()
            .next()
            .filter(|key| key.to_lowercase() == "_all_")
            .cloned();
        if let Some(alias) = alias {
            let value = mapping.remove(&alias).expect("key was just observed");
            mapping.insert(environment.to_owned(), value);
        }
    }
}
