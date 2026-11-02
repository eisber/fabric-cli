// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::yaml::{self, YamlLimits, YamlPolicy};

pub(super) async fn check<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    base: &str,
    names: &[String],
    initial: bool,
) -> Result<(), WorkflowFailure> {
    let mut attempt = 1u32;
    loop {
        // The source emits a trailing slash; FabricOperation rejects empty path
        // segments, so the equivalent collection resource is used without it.
        let environments = io.list(&format!("{base}/environments")).await?;
        let mut running = false;
        let mut completed = HashSet::new();
        for environment in environments {
            let Some(name) = environment.get("displayName").and_then(Value::as_str) else {
                continue;
            };
            if !names.iter().any(|selected| selected == name) {
                continue;
            }
            let state = environment
                .pointer("/properties/publishDetails/state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            if state == "running" {
                running = true;
            }
            if state == "success" {
                completed.insert(name.to_owned());
            }
            if !initial
                && !matches!(
                    state.as_str(),
                    "running" | "success" | "failed" | "cancelled"
                )
            {
                return Err(io.error(OperationErrorKind::InvalidResponse).into());
            }
            if !initial && matches!(state.as_str(), "failed" | "cancelled") {
                return Err(io
                    .error(OperationErrorKind::OperationFailed {
                        status: state,
                        error: None,
                    })
                    .into());
            }
        }
        if !initial && names.iter().any(|name| !completed.contains(name)) {
            running = true;
        }
        if !running {
            if !initial {
                for effect in &mut io.result.effects {
                    if effect.path.starts_with(&format!("{base}/environments/"))
                        && effect.path.ends_with("/staging/publish?beta=False")
                    {
                        effect.pending = false;
                    }
                }
            }
            return Ok(());
        }
        io.wait(source_delay(attempt, 120)).await?;
        attempt = attempt.saturating_add(1);
    }
}

pub(super) async fn preprocess<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    publication: Publication<'_>,
    item: &RepositoryItem,
    path: &str,
    content: &str,
) -> Result<String, WorkflowFailure> {
    let Publication {
        config,
        context,
        base,
        ..
    } = publication;
    if path != "Setting/Sparkcompute.yml" || !content.contains("instance_pool_id") {
        return Ok(content.into());
    }
    let mut document = yaml::decode(content, YamlPolicy::Config, YamlLimits::default())?;
    if !document.is_object() {
        return Ok(content.into());
    }
    if let Some(pool_id) = document.get("instance_pool_id").cloned()
        && let Some(rules) = context
            .parameter_document
            .as_ref()
            .and_then(|document| document.get("spark_pool"))
            .and_then(Value::as_array)
    {
        let pools = io.list(&format!("{base}/spark/pools")).await?;
        for rule in rules {
            if rule.get("instance_pool_id") != Some(&pool_id)
                || !super::super::parameters::matches_filter(rule.get("item_name"), &item.name)
            {
                continue;
            }
            let replacement = super::super::parameters::environment_value(
                rule.get("replace_value"),
                &config.environment,
            )
            .ok_or_else(|| invalid("Spark pool target environment was not found"))?;
            let name = replacement
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Spark pool name is required"))?;
            let kind = replacement
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Spark pool type is required"))?;
            let pool = pools
                .iter()
                .find(|pool| {
                    pool.get("name").and_then(Value::as_str) == Some(name)
                        && pool.get("type").and_then(Value::as_str) == Some(kind)
                })
                .and_then(|pool| pool.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Custom Spark pool could not be resolved"))?;
            document["instance_pool_id"] = pool.into();
            break;
        }
    }
    Ok(emit_yaml(&document))
}

fn emit_yaml(value: &Value) -> String {
    fn scalar(value: &Value, mapping_key: bool) -> String {
        match value {
            Value::Null => "null".into(),
            Value::String(value) => {
                if value.chars().any(char::is_control) {
                    return fabric_core_json::json_dumps(&Value::String(value.clone()));
                }
                let ambiguous = value.is_empty()
                    || value.trim() != value
                    || value.contains([':', '#', '\n', '\r', '\'', '"', '[', ']', '{', '}', ','])
                    || ["true", "false", "yes", "no", "on", "off", "null", "~"]
                        .contains(&value.to_lowercase().as_str())
                    || value.parse::<f64>().is_ok()
                    || !yaml::decode(
                        &if mapping_key {
                            format!("{value}: scalar\n")
                        } else {
                            format!("value: {value}\n")
                        },
                        YamlPolicy::Config,
                        YamlLimits::default(),
                    )
                    .is_ok_and(|document| {
                        if mapping_key {
                            document.as_object().is_some_and(|mapping| {
                                mapping.len() == 1
                                    && mapping.get(value).and_then(Value::as_str) == Some("scalar")
                            })
                        } else {
                            document.get("value") == Some(&Value::String(value.clone()))
                        }
                    });
                if ambiguous {
                    format!("'{}'", value.replace('\'', "''"))
                } else {
                    value.clone()
                }
            }
            value => fabric_core_json::json_dumps(value),
        }
    }
    fn emit(value: &Value, depth: usize, output: &mut String) {
        match value {
            Value::Object(mapping) => {
                for (key, value) in mapping {
                    output.push_str(&" ".repeat(depth));
                    output.push_str(&scalar(&Value::String(key.clone()), true));
                    output.push(':');
                    if value.is_object() || value.is_array() {
                        if value.as_array().is_some_and(Vec::is_empty) {
                            output.push_str(" []\n");
                        } else if value.as_object().is_some_and(serde_json::Map::is_empty) {
                            output.push_str(" {}\n");
                        } else {
                            output.push('\n');
                            emit(
                                value,
                                if value.is_array() { depth } else { depth + 2 },
                                output,
                            );
                        }
                    } else {
                        output.push(' ');
                        output.push_str(&scalar(value, false));
                        output.push('\n');
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    output.push_str(&" ".repeat(depth));
                    output.push_str("- ");
                    if value.is_object() || value.is_array() {
                        output.push('\n');
                        emit(value, depth + 2, output);
                    } else {
                        output.push_str(&scalar(value, false));
                        output.push('\n');
                    }
                }
            }
            value => {
                output.push_str(&scalar(value, false));
                output.push('\n');
            }
        }
    }
    let mut output = String::new();
    emit(value, 0, &mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_settings_retain_indicator_strings_keys_and_scalar_types() {
        let document = serde_json::json!({
            "instance_pool_id": "22222222-2222-2222-2222-222222222222",
            "spark_conf": {
                "*": "*",
                "<<": "literal",
                "anchor": "&value",
                "tag": "!value",
                "block": "|",
                "fold": ">",
                "directive": "%YAML",
                "reserved": "@value",
                "backtick": "`value",
                "octal": "012",
                "hex": "0x12",
                "newline": "first\nsecond",
                "control": "first\tsecond",
                "boolean": true,
                "number": 12,
                "nothing": null,
            },
        });
        assert_eq!(
            yaml::decode(
                &emit_yaml(&document),
                YamlPolicy::Config,
                YamlLimits::default()
            )
            .unwrap(),
            document
        );
    }
}
