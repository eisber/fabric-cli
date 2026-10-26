// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native embedded definition bytes pinned to Python cf67fd52, never runtime files.

use crate::MutationError;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};

pub(crate) fn semantic_model() -> Value {
    parts(&[
        (
            ".platform",
            include_bytes!("../templates/Blank.SemanticModel/.platform"),
        ),
        (
            "definition.pbism",
            include_bytes!("../templates/Blank.SemanticModel/definition.pbism"),
        ),
        (
            "definition/database.tmdl",
            include_bytes!("../templates/Blank.SemanticModel/definition/database.tmdl"),
        ),
        (
            "definition/model.tmdl",
            include_bytes!("../templates/Blank.SemanticModel/definition/model.tmdl"),
        ),
        (
            "definition/tables/Table.tmdl",
            include_bytes!("../templates/Blank.SemanticModel/definition/tables/Table.tmdl"),
        ),
    ])
}

pub(crate) fn generic_mirror() -> Value {
    parts(&[(
        "mirroring.json",
        include_bytes!("../templates/MirroredDatabase.GenericMirror/mirroring.json"),
    )])
}

pub(crate) fn report(semantic_model: Option<&Value>) -> Result<Value, MutationError> {
    let files: &[(&str, &[u8])] = &[
        (
            ".platform",
            include_bytes!("../templates/Blank.Report/.platform"),
        ),
        (
            "definition.pbir",
            include_bytes!("../templates/Blank.Report/definition.pbir"),
        ),
        (
            "definition/report.json",
            include_bytes!("../templates/Blank.Report/definition/report.json"),
        ),
        (
            "definition/version.json",
            include_bytes!("../templates/Blank.Report/definition/version.json"),
        ),
        (
            "definition/pages/pages.json",
            include_bytes!("../templates/Blank.Report/definition/pages/pages.json"),
        ),
        (
            "definition/pages/b8c5fb8d635f898326c6/page.json",
            include_bytes!(
                "../templates/Blank.Report/definition/pages/b8c5fb8d635f898326c6/page.json"
            ),
        ),
        (
            "StaticResources/SharedResources/BaseThemes/CY24SU10.json",
            include_bytes!(
                "../templates/Blank.Report/StaticResources/SharedResources/BaseThemes/CY24SU10.json"
            ),
        ),
    ];
    let mut result = parts(files);
    if let Some(model) = semantic_model.filter(|value| crate::create::truthy(value)) {
        let mut definition = template_json(files[1].1)?;
        definition["datasetReference"]["byPath"] = Value::Null;
        definition["datasetReference"]["byConnection"] = json!({
            "connectionString":"Data Source=powerbi://api.powerbi.com/v1.0/myorg/mkdir;Initial Catalog=r3;Integrated Security=ClaimsToken",
            "pbiServiceModelId":null,
            "pbiModelVirtualServerName":"sobe_wowvirtualserver",
            "pbiModelDatabaseName":model,
            "name":"EntityDataSource",
            "connectionType":"pbiServiceXmlaStyleLive",
        });
        result["parts"][1]["payload"] = encoded_json(&definition)?.into();
    }
    Ok(result)
}

pub(crate) fn mirror(params: &Map<String, Value>) -> Result<Value, MutationError> {
    let mirror_type = match params
        .get("mirrortype")
        .filter(|value| crate::create::truthy(value))
    {
        Some(Value::String(value)) => value.to_lowercase(),
        Some(value) => return Err(source_attribute(value, "lower")),
        None => "genericmirror".into(),
    };
    if mirror_type == "genericmirror" {
        return Ok(generic_mirror());
    }
    let bytes: &[u8] = match mirror_type.as_str() {
        "azuresql" => {
            include_bytes!("../templates/MirroredDatabase.AzureSQLDatabase/mirroring.json")
        }
        "azuresqlmi" => include_bytes!("../templates/MirroredDatabase.AzureSqlMI/mirroring.json"),
        "snowflake" => include_bytes!("../templates/MirroredDatabase.Snowflake/mirroring.json"),
        "cosmosdb" => include_bytes!("../templates/MirroredDatabase.CosmosDb/mirroring.json"),
        _ => include_bytes!("../templates/MirroredDatabase.GenericMirror/mirroring.json"),
    };
    let mut data = template_json(bytes)?;
    data["properties"]["source"]["typeProperties"]["connection"] =
        params.get("connectionid").cloned().unwrap_or(Value::Null);
    if let Some(database) = params
        .get("database")
        .filter(|value| crate::create::truthy(value))
    {
        data["properties"]["source"]["typeProperties"]["database"] = database.clone();
    }
    data["properties"]["target"]["typeProperties"]["defaultSchema"] = params
        .get("defaultschema")
        .cloned()
        .unwrap_or_else(|| "dbo".into());
    if let Some(tables) = params
        .get("mountedtables")
        .filter(|value| crate::create::truthy(value))
    {
        let tables = tables
            .as_str()
            .ok_or_else(|| source_attribute(tables, "split"))?;
        let mut mounted = Vec::new();
        for table in tables.split(',') {
            let names = table.split('.').collect::<Vec<_>>();
            if names.len() != 2 {
                return Err(MutationError::new(
                    "UnexpectedError",
                    if names.len() < 2 {
                        format!(
                            "not enough values to unpack (expected 2, got {})",
                            names.len()
                        )
                    } else {
                        "too many values to unpack (expected 2)".into()
                    },
                ));
            }
            mounted.push(
                json!({"source":{"typeProperties":{"schemaName":names[0],"tableName":names[1]}}}),
            );
        }
        data["properties"]["mountedTables"] = Value::Array(mounted);
    }
    Ok(json!({"parts":[{
        "path":"mirroring.json","payload":encoded_json(&data)?,"payloadType":"InlineBase64",
    }]}))
}

pub(crate) fn mounted_factory(params: &Map<String, Value>) -> Result<Value, MutationError> {
    let mut values = Vec::new();
    for key in ["subscriptionid", "resourcegroup", "factoryname"] {
        let value = params.get(key).ok_or_else(|| parameter_type(key))?;
        values.push(match value {
            Value::String(value) => value.clone(),
            _ => crate::errors::python_repr(value)
                .map_err(|()| MutationError::new("InvalidJson", "Invalid JSON format"))?,
        });
    }
    let data = json!({"dataFactoryResourceId":format!("/subscriptions/{}/resourceGroups/{}/providers/Microsoft.DataFactory/factories/{}",values[0],values[1],values[2])});
    Ok(json!({"parts":[{
        "path":"mountedDataFactory-content.json","payload":encoded_json(&data)?,"payloadType":"InlineBase64",
    }]}))
}

fn template_json(bytes: &[u8]) -> Result<Value, MutationError> {
    serde_json::from_slice(bytes)
        .map_err(|_| MutationError::new("InvalidJson", "Invalid embedded definition JSON"))
}

fn encoded_json(data: &Value) -> Result<String, MutationError> {
    let data = fabric_cli_output::json_dumps(data)
        .map_err(|_| MutationError::new("InvalidJson", "Invalid JSON format"))?;
    Ok(STANDARD.encode(data.as_bytes()))
}

fn parameter_type(name: &str) -> MutationError {
    MutationError::new(
        "InvalidInput",
        format!("Invalid {name} definition parameter type"),
    )
}

fn source_attribute(value: &Value, attribute: &str) -> MutationError {
    let kind = match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::String(_) => "str",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    };
    MutationError::new(
        "UnexpectedError",
        format!("'{kind}' object has no attribute '{attribute}'"),
    )
}

fn parts(files: &[(&str, &[u8])]) -> Value {
    json!({"parts":files.iter().map(|(path,bytes)|json!({
        "path":path,"payload":STANDARD.encode(bytes),"payloadType":"InlineBase64",
    })).collect::<Vec<_>>()})
}
