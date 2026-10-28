// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic public preflight caller; records local collaborators, never does I/O.

use fabric_cli_context::ItemType;
use fabric_cli_mutations::{
    MutationError, OneLakeResourceKind, OneLakeWriteError, OneLakeWriteTarget, file_destination,
    validate_copy_destination,
};
use fabric_sdk_onelake::OneLakePath;
use serde_json::{Value, json};
use std::io::{self, BufRead};

fn command(code: &str, message: &str) -> OneLakeWriteError {
    OneLakeWriteError::Command(MutationError::new(code, message))
}

fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let spec = &input["target"];
    let path = spec["path"].as_str().ok_or("missing destination path")?;
    let target = OneLakeWriteTarget {
        path: OneLakePath::new("workspace", "item", path)?,
        item_type: ItemType::parse(spec["item_type"].as_str().ok_or("missing item type")?)?,
        kind: match spec["kind"].as_str().ok_or("missing target kind")? {
            "directory" => OneLakeResourceKind::Directory,
            "shortcut" => OneLakeResourceKind::Shortcut,
            "table" => OneLakeResourceKind::Table,
            "undefined" => OneLakeResourceKind::Undefined,
            "file" => OneLakeResourceKind::File,
            _ => return Err("invalid target kind".into()),
        },
        display_path: format!("/Team.Workspace/Lake.Lakehouse/{path}"),
    };
    let source = input["source"].as_str().ok_or("missing source kind")?;
    let name = input["source_name"].as_str().ok_or("missing source name")?;
    let mut trace = Vec::<Value>::new();
    let mut resolved = None;
    let result = (|| {
        validate_copy_destination(&target)?;
        trace.push(json!("is_directory"));
        if source == "directory" {
            return Err(command("NotSupported", "Recursive copy not supported"));
        }
        trace.push(json!("is_file"));
        if source == "missing" {
            return Err(command("InvalidPath", "Invalid source, expected file"));
        }
        let target = file_destination(&target, name)?;
        resolved = Some(target.path.relative().to_owned());
        trace.push(json!({"progress":format!("Copying 'local-source' \u{2192} '{}'...",target.display_path)}));
        trace.push(json!("read"));
        if source == "read_error" {
            return Err(command(
                "InvalidPath",
                "Error reading file: synthetic denied",
            ));
        }
        if source == "empty" {
            return Err(command("InvalidInput", "Invalid copy, source is empty"));
        }
        trace.push(json!("upload"));
        Ok(())
    })();
    Ok(
        json!({"trace":trace,"destination":resolved,"error":result.err().map(|error| {
            let output=error.output();
            json!({"code":output.error_code,"message":output.message})
        })}),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?)?);
    }
    Ok(())
}
