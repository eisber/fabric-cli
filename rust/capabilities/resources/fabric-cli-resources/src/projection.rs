// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::CommandError;
use fabric_sdk_access::Resource;
use serde_json::{Value, json};

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a Value, CommandError> {
    value
        .get(key)
        .ok_or_else(|| CommandError::new("InvalidEntriesFormat", "Invalid ACL entries format"))
}

fn fallback(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or_else(|| json!("Unknown"))
}

fn object(value: &Value) -> Result<&Value, CommandError> {
    if value.is_object() {
        Ok(value)
    } else {
        Err(CommandError::new(
            "InvalidEntriesFormat",
            "Invalid ACL entries format",
        ))
    }
}

/// Translates resource-specific source rows before sorting, column selection/query.
/// Queries see the full transformed dictionaries, not the already-trimmed columns.
pub fn project_acls(
    resource: &Resource,
    rows: Vec<Value>,
    long: bool,
    query: Option<&str>,
) -> Result<Value, CommandError> {
    let mut transformed = Vec::new();
    let (columns, sort_key): (Vec<&str>, &str) = match resource {
        Resource::Workspace(_) => {
            for acl in rows {
                let principal = match acl.get("principal") {
                    Some(value) => object(value)?,
                    None => &Value::Null,
                };
                let identity = if principal
                    .get("userDetails")
                    .is_some_and(crate::python_truthy)
                {
                    object(&principal["userDetails"])?.get("userPrincipalName")
                } else if principal
                    .get("servicePrincipalDetails")
                    .is_some_and(crate::python_truthy)
                {
                    object(&principal["servicePrincipalDetails"])?.get("aadAppId")
                } else if principal
                    .get("groupDetails")
                    .is_some_and(crate::python_truthy)
                {
                    principal.get("displayName")
                } else {
                    None
                };
                transformed.push(json!({
                    "name":fallback(principal,"displayName"),"type":fallback(principal,"type"),
                    "identity":identity,"objectId":fallback(&acl,"id"),"acl":fallback(&acl,"role")
                }));
            }
            (
                if long {
                    vec!["acl", "identity", "type", "objectId", "name"]
                } else {
                    vec!["acl", "identity", "type"]
                },
                "acl",
            )
        }
        Resource::Gateway(_) | Resource::Connection(_) => {
            for acl in rows {
                let p = required(&acl, "principal")?;
                transformed.push(
                    json!({"id":required(&acl,"id")?,"role":required(&acl,"role")?,
                    "principalId":required(p,"id")?,"principalType":required(p,"type")?}),
                );
            }
            (
                if long {
                    vec!["id", "role", "principalId", "principalType"]
                } else {
                    vec!["role", "principalId", "principalType"]
                },
                "role",
            )
        }
        Resource::Item { .. } => {
            for acl in rows {
                let p = required(&acl, "principal")?;
                let identity = if p.get("userDetails").is_some() {
                    object(&p["userDetails"])?.get("userPrincipalName")
                } else if p.get("servicePrincipalDetails").is_some() {
                    object(&p["servicePrincipalDetails"])?.get("aadAppId")
                } else if p.get("groupDetails").is_some() {
                    object(&p["groupDetails"])?.get("groupType")
                } else {
                    None
                };
                transformed.push(json!({"name":required(p,"displayName")?,"type":required(p,"type")?,
                    "identity":identity,"id":required(p,"id")?,"acl":required(required(&acl,"itemAccessDetails")?,"permissions")?}));
            }
            (
                if long {
                    vec!["id", "name", "acl", "identity", "type"]
                } else {
                    vec!["acl", "identity", "type"]
                },
                "acl",
            )
        }
        Resource::LakehouseDataRoles { .. } => {
            for role in rows {
                let name = required(&role, "name")?;
                let rules = required(&role, "decisionRules")?;
                let members = object(required(&role, "members")?)?;
                for (field, identity, kind) in [
                    ("microsoftEntraMembers", "objectId", "microsoftEntraMember"),
                    ("fabricItemMembers", "sourcePath", "fabricItemMember"),
                ] {
                    if let Some(members) = members.get(field) {
                        for member in members.as_array().ok_or_else(|| {
                            CommandError::new("InvalidEntriesFormat", "Invalid ACL entries format")
                        })? {
                            transformed.push(json!({"identity":required(member,identity)?,"details":rules,"acl":name,"type":kind}));
                        }
                    }
                }
            }
            (
                if long {
                    vec!["id", "name", "acl", "identity", "type"]
                } else {
                    vec!["acl", "identity", "type"]
                },
                "acl",
            )
        }
    };
    // Python compares strings or homogeneous string lists lexicographically.
    fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
        match (a, b) {
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            (Value::Array(a), Value::Array(b)) => {
                for (a, b) in a.iter().zip(b) {
                    match compare(a, b)? {
                        std::cmp::Ordering::Equal => {}
                        order => return Some(order),
                    }
                }
                Some(a.len().cmp(&b.len()))
            }
            _ => None,
        }
    }
    for row in &transformed {
        if compare(&row[sort_key], &row[sort_key]).is_none() {
            return Err(CommandError::new(
                "InvalidEntriesFormat",
                "Invalid ACL sort field",
            ));
        }
    }
    if let Some(first) = transformed.first()
        && transformed
            .iter()
            .any(|row| compare(&first[sort_key], &row[sort_key]).is_none())
    {
        return Err(CommandError::new(
            "InvalidEntriesFormat",
            "Inconsistent ACL sort fields",
        ));
    }
    transformed.sort_by(|a, b| compare(&a[sort_key], &b[sort_key]).expect("validated sort fields"));
    let value = Value::Array(transformed);
    if let Some(query) = query.filter(|q| !q.is_empty()) {
        fabric_cli_query::search(&value, query).map_err(|_| {
            CommandError::new(
                "InvalidInput",
                "Invalid jmespath query (https://jmespath.org)",
            )
        })
    } else {
        Ok(Value::Array(
            value
                .as_array()
                .expect("array")
                .iter()
                .map(|row| {
                    Value::Object(
                        columns
                            .iter()
                            .filter_map(|key| row.get(*key).map(|v| ((*key).into(), v.clone())))
                            .collect(),
                    )
                })
                .collect(),
        ))
    }
}
