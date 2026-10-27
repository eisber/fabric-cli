// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli::parser::{Invocation, ReadCommand, ReadKind, parse_args};
use serde_json::{Value, json};

pub fn meaning(command: &ReadCommand) -> Value {
    json!({
        "kind": if command.kind == ReadKind::List { "list" } else { "get" },
        "name": command.name,
        "path": command.path,
        "query": command.query,
        "long": command.long,
        "all": command.all,
        "output": command.output,
        "verbose": command.verbose,
        "force": command.force,
        "output_format": command.output_format,
    })
}

pub fn observe(input: &Value) -> Option<Value> {
    if input["parse_only"] != true {
        return None;
    }
    let args = std::iter::once("fab-rs").chain(
        input["command"]
            .as_array()
            .expect("raw command argv")
            .iter()
            .map(|value| value.as_str().expect("argument string")),
    );
    let result = match parse_args(args) {
        Ok(Invocation::Read(command)) => json!({"read": meaning(&command)}),
        Ok(Invocation::Display(_)) => json!({"display": true}),
        Ok(
            Invocation::JobRead(_) | Invocation::JobRun(_) | Invocation::JobMutation(_)
            | Invocation::Local(_) | Invocation::Access(_) | Invocation::Resource(_)
            | Invocation::Transfer(_),
        ) => {
            json!({"error": true})
        }
        #[cfg(feature = "windows-broker")]
        Ok(Invocation::Auth(_)) => json!({"error": true}),
        Err(_) => json!({"error": true}),
    };
    Some(json!({"id": input["id"], "parse": result}))
}
