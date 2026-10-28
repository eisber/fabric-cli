// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! JSON-line consumer of the real local adapter and shared HTTP, with no live I/O.
//! The caller supplies an existing disposable fixture directory in every row.

#[path = "../tests/support/mod.rs"]
mod support;

use fabric_cli_mutations::OneLakeResourceKind;
use fabric_cli_storage_files::{OsFileSystem, StorageFilesHost};
use fabric_sdk_onelake::{OneLakeLimits, OneLakeWriteLimits};
use serde_json::{Value, json};
use std::{
    future::pending,
    io::{self, BufRead},
    path::PathBuf,
};
use support::{FaultLocal, Ui, response, setup, source};

fn bytes(value: &Value) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    value
        .as_array()
        .ok_or("missing bytes")?
        .iter()
        .map(|byte| {
            byte.as_u64()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or_else(|| "invalid byte".into())
        })
        .collect()
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let replies = input["replies"]
        .as_array()
        .ok_or("missing replies")?
        .iter()
        .map(|reply| {
            let status = reply["status"]
                .as_u64()
                .and_then(|v| u16::try_from(v).ok())
                .ok_or("invalid status")?;
            Ok(response(status, &bytes(&reply["body"])?))
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let (client, executor, network) = setup(replies);
    let local = FaultLocal {
        inner: OsFileSystem::new(
            PathBuf::from(input["root"].as_str().ok_or("missing fixture root")?),
            tokio::runtime::Handle::current(),
        )
        .await?,
        read_failure: input["read_failure"].as_bool().unwrap_or(false),
        write_after: input["write_after"]
            .as_u64()
            .map(usize::try_from)
            .transpose()?,
        short_success: false,
    };
    let mut ui = Ui::default();
    let target = PathBuf::from(input["target"].as_str().ok_or("missing target")?);
    let mut remote = source(
        input["source"].as_str().unwrap_or("Files/source.txt"),
        match input["kind"].as_str().unwrap_or("file") {
            "file" => OneLakeResourceKind::File,
            "directory" => OneLakeResourceKind::Directory,
            "shortcut" => OneLakeResourceKind::Shortcut,
            "table" => OneLakeResourceKind::Table,
            "undefined" => OneLakeResourceKind::Undefined,
            _ => return Err("invalid kind".into()),
        },
    );
    remote.item_type =
        fabric_cli_context::ItemType::parse(input["item_type"].as_str().unwrap_or("Lakehouse"))?;
    let mut host = StorageFilesHost {
        client: &client,
        executor: &executor,
        local: &local,
        ui: &mut ui,
        read_limits: OneLakeLimits::default(),
    };
    let result = if input["direction"].as_str() == Some("upload") {
        host.upload(&target, &remote, OneLakeWriteLimits::default(), pending())
            .await
            .map(|result| (result.outputs, result.local_path))
    } else {
        host.download(&remote, &target, pending())
            .await
            .map(|result| (result.outputs, result.local_path))
    };
    let (outputs, error, local_path) = match result {
        Ok((outputs, local_path)) => (
            outputs
                .iter()
                .map(|v| v.message.clone())
                .collect::<Vec<_>>(),
            None,
            local_path,
        ),
        Err(error) => {
            let output = error.output();
            (
                Vec::new(),
                Some(json!({"code":output.error_code, "message":output.message})),
                error.local_path,
            )
        }
    };
    if !network.replies.lock().unwrap().is_empty() {
        return Err("unused synthetic replies".into());
    }
    Ok(json!({
        "requests":*network.requests.lock().unwrap(), "notices":ui.notices, "prompts":ui.prompts,
        "outputs":outputs, "error":error, "local_path":local_path,
    }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?).await?);
    }
    Ok(())
}
