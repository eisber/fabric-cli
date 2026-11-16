// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resource command parsing and local payload preparation.

use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_cli_command_contracts::CommandError;
use fabric_core::{Audience, Clock, HttpExecutor, MAX_REQUEST_BYTES, OperationContext};
use fabric_resources::Selector;
use serde_json::{Map, Value};
use std::{io::Read, path::Path};

pub(crate) struct Prepared {
    pub kind: &'static str,
    pub verb: &'static str,
    pub target: Option<Selector>,
    pub workspace: Option<Selector>,
    pub body: Vec<u8>,
    pub query: Option<String>,
    pub force: bool,
    pub asynchronous: bool,
    pub polling: fabric_sdk_api::LroConfig,
}

fn invalid(message: &str) -> CommandError {
    CommandError::new("InvalidInput", message)
}

pub(crate) fn command(kind: &'static str) -> Command {
    let mut parent = Command::new(kind)
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .arg(super::help_arg())
        .subcommand_required(true);
    for verb in ["list", "get", "create", "update", "delete"] {
        let mut leaf = Command::new(verb)
            .disable_help_flag(true)
            .arg(super::help_arg())
            .arg(Arg::new("query").long("query").help(
                "JMESPath on resource data; list single-field queries keep one object per row",
            ));
        if kind == "item" {
            leaf = leaf.arg(
                Arg::new("workspace")
                    .long("workspace")
                    .required(matches!(verb, "list" | "create")),
            );
        }
        if verb != "list" {
            leaf = leaf.arg(Arg::new("target").required(verb != "create"));
        }
        if verb == "create" || verb == "update" {
            leaf = leaf.arg(
                Arg::new("payload")
                    .long("payload")
                    .required(verb == "update"),
            );
        }
        if verb == "delete" {
            leaf = leaf.arg(Arg::new("force").long("force").action(ArgAction::SetTrue));
        }
        if matches!(verb, "create" | "update" | "delete") {
            leaf = leaf
                .arg(Arg::new("async").long("async").action(ArgAction::SetTrue))
                .arg(Arg::new("polling-config").long("polling-config"));
        }
        parent = parent.subcommand(leaf);
    }
    parent
}

fn payload(text: Option<&str>) -> Result<Map<String, Value>, CommandError> {
    let Some(text) = text else {
        return Ok(Map::new());
    };
    let bytes = if let Some(name) = text.strip_prefix('@') {
        let path = Path::new(name);
        let metadata =
            std::fs::metadata(path).map_err(|_| invalid("Cannot inspect payload file"))?;
        if !metadata.is_file() || metadata.len() > MAX_REQUEST_BYTES as u64 {
            return Err(invalid("Payload must be a bounded regular file"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|_| invalid("Cannot open payload file"))?;
        if !file
            .metadata()
            .map_err(|_| invalid("Cannot inspect payload file"))?
            .is_file()
        {
            return Err(invalid("Payload must be a regular file"));
        }
        let mut bytes = Vec::new();
        file.take((MAX_REQUEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("Cannot read payload file"))?;
        bytes
    } else {
        if text.len() > MAX_REQUEST_BYTES {
            return Err(invalid("Payload exceeds allocation bound"));
        }
        text.as_bytes().to_vec()
    };
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(invalid("Payload exceeds allocation bound"));
    }
    match fabric_core_json::decode(&bytes) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(invalid("Payload must be a UTF-8 JSON object")),
    }
}

pub(crate) fn prepare(kind: &'static str, matches: &ArgMatches) -> Result<Prepared, CommandError> {
    let (verb, args) = matches
        .subcommand()
        .ok_or_else(|| invalid("A resource action is required"))?;
    let verb = match verb {
        "list" => "list",
        "get" => "get",
        "create" => "create",
        "update" => "update",
        "delete" => "delete",
        _ => return Err(invalid("Invalid resource action")),
    };
    let text = |key| args.get_one::<String>(key).map(String::as_str);
    let target = if verb == "list" {
        None
    } else {
        text("target").map(Selector::parse).transpose()?
    };
    if verb == "create" && matches!(&target, Some(Selector::Id(_))) {
        return Err(invalid("A creation name must not be a GUID"));
    }
    finish_prepare(kind, args, verb, target)
}

pub(crate) async fn execute(
    prepared: Prepared,
    client: &dyn HttpExecutor,
    clock: &dyn Clock,
    context: &OperationContext,
    observer: &dyn fabric_sdk_api::PollObserver,
    confirmer: &super::ConsoleConfirmation,
) -> Result<Value, CommandError> {
    let workspace_id = match &prepared.workspace {
        Some(selector) => {
            Some(fabric_resources::resolve(client, clock, context, "workspaces", selector).await?)
        }
        None if prepared.kind == "item" => {
            let Some(Selector::Id(id)) = &prepared.target else {
                return Err(invalid("Item name requires --workspace"));
            };
            Some(fabric_resources::find_item_workspace(client, clock, context, id).await?)
        }
        None => None,
    };
    let collection = match &workspace_id {
        Some(id) => format!("workspaces/{id}/items"),
        None => "workspaces".to_owned(),
    };
    let target_id = match (&prepared.target, prepared.verb) {
        (Some(selector), "get" | "update" | "delete") => {
            Some(fabric_resources::resolve(client, clock, context, &collection, selector).await?)
        }
        _ => None,
    };
    if prepared.verb == "delete" && !prepared.force {
        context.remaining_at(clock.now())?;
        let id = target_id
            .as_deref()
            .ok_or_else(|| invalid("Missing resource target"))?;
        let scope = workspace_id
            .as_ref()
            .map(|id| format!(" in workspace {id}"))
            .unwrap_or_default();
        confirmer
            .confirm(format!("Delete {} {id}{scope}? [Y/n] ", prepared.kind))
            .await?;
        context.remaining_at(clock.now())?;
    }
    let data = if prepared.verb == "list" {
        fabric_resources::list(client, clock, context, &collection).await?
    } else {
        let endpoint = match target_id {
            Some(id) => format!("{collection}/{id}"),
            None => collection,
        };
        let method = match prepared.verb {
            "get" => "GET",
            "create" => "POST",
            "update" => "PATCH",
            "delete" => "DELETE",
            _ => return Err(invalid("Invalid resource action")),
        };
        let initial =
            fabric_resources::request(client, context, method, &endpoint, prepared.body).await?;
        let recovery =
            match fabric_core::operation_handle(Audience::Fabric, initial.status, &initial.headers)
            {
                Ok(handle) => handle.map(|handle| handle.recovery()),
                Err(error) => error.recovery,
            };
        let attach = |mut error: CommandError| {
            if error.recovery.is_none() {
                error.recovery = recovery
                    .as_ref()
                    .and_then(|value| serde_json::to_value(value).ok());
            }
            error
        };
        let response = if prepared.asynchronous {
            initial
        } else {
            fabric_cli_api::validate_response_body(&initial.body).map_err(&attach)?;
            fabric_sdk_api::complete_observed(
                client,
                clock,
                Audience::Fabric,
                initial,
                context.cancellation.clone(),
                prepared.polling,
                observer,
            )
            .await
            .map_err(|error| attach(error.into()))?
        };
        if !(200..300).contains(&response.status) {
            return Err(attach(CommandError::new(
                "Service",
                format!(
                    "Fabric resource request failed with HTTP {}",
                    response.status
                ),
            )));
        }
        let empty = response.body.iter().all(u8::is_ascii_whitespace);
        let body = if empty && response.status == 204 && prepared.verb != "get" {
            Value::Null
        } else if empty && response.status == 202 {
            Value::Null
        } else if empty {
            return Err(attach(CommandError::new(
                "InvalidJson",
                "Empty resource response",
            )));
        } else {
            fabric_core_json::decode(&response.body).map_err(|_| {
                attach(CommandError::new(
                    "InvalidJson",
                    "Malformed resource response JSON",
                ))
            })?
        };
        if response.status == 202 {
            let mut accepted = Map::new();
            accepted.insert("status_code".into(), Value::from(202));
            accepted.insert("status".into(), Value::String("Accepted".into()));
            accepted.insert("body".into(), body);
            if let Some(recovery) = recovery.as_ref() {
                if let Ok(value) = serde_json::to_value(recovery) {
                    accepted.insert("recovery".into(), value);
                }
            }
            Value::Object(accepted)
        } else {
            if prepared.verb == "get" && !body.is_object() {
                return Err(attach(CommandError::new(
                    "Protocol",
                    "Resource response must be an object",
                )));
            }
            body
        }
    };
    match prepared.query.as_deref().filter(|query| !query.is_empty()) {
        Some(query) => {
            let result = if prepared.verb == "list" {
                fabric_cli_query::search_list(&data, query)
            } else {
                fabric_cli_query::search(&data, query)
            };
            result.map_err(|_| invalid("Invalid JMESPath query"))
        }
        None => Ok(data),
    }
}

fn finish_prepare(
    kind: &'static str,
    args: &ArgMatches,
    verb: &'static str,
    target: Option<Selector>,
) -> Result<Prepared, CommandError> {
    let text = |key| args.get_one::<String>(key).map(String::as_str);
    let workspace = if kind == "item" {
        text("workspace").map(Selector::parse).transpose()?
    } else {
        None
    };
    if kind == "item" && workspace.is_none() && !matches!(&target, Some(Selector::Id(_))) {
        return Err(invalid("Item name requires --workspace"));
    }
    let query = text("query").map(str::to_owned);
    if let Some(value) = query.as_deref().filter(|value| !value.is_empty()) {
        if value.len() > 16 * 1024 || fabric_cli_query::validate(value).is_err() {
            return Err(invalid("Invalid or oversized JMESPath query"));
        }
    }
    let mutation = matches!(verb, "create" | "update" | "delete");
    let asynchronous = mutation && args.get_flag("async");
    let polling_text = if mutation {
        text("polling-config")
    } else {
        None
    };
    if asynchronous && polling_text.is_some() {
        return Err(invalid("--async cannot be combined with --polling-config"));
    }
    let polling = fabric_cli_api::parse_polling_config(polling_text)?;
    let mut body = if matches!(verb, "create" | "update") {
        payload(text("payload"))?
    } else {
        Map::new()
    };
    if matches!(verb, "create" | "update") {
        if body.contains_key("id") {
            return Err(invalid(
                "Resource IDs belong in the target, not the payload",
            ));
        }
        if kind == "item" && body.contains_key("workspaceId") {
            return Err(invalid(
                "workspaceId is routing information; use --workspace",
            ));
        }
        for key in ["displayName", "description"] {
            if body.get(key).is_some_and(|value| !value.is_string()) {
                return Err(invalid("Resource string property has an invalid type"));
            }
        }
        if verb == "create" {
            if let Some(Selector::Name(name)) = &target {
                if body
                    .get("displayName")
                    .is_some_and(|value| value.as_str() != Some(name))
                {
                    return Err(invalid("Creation name and payload displayName must match"));
                }
                body.insert("displayName".into(), Value::String(name.clone()));
            }
            if body
                .get("displayName")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(invalid("Creation requires displayName"));
            }
            if kind == "item"
                && body
                    .get("type")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(invalid("Item creation requires type"));
            }
            if kind == "item" {
                for key in [
                    "definition",
                    "creationPayload",
                    "options",
                    "sensitivityLabelSettings",
                ] {
                    if body.get(key).is_some_and(|value| !value.is_object()) {
                        return Err(invalid("Resource object property has an invalid type"));
                    }
                }
                if body.contains_key("definition") && body.contains_key("creationPayload") {
                    return Err(invalid(
                        "definition and creationPayload are mutually exclusive",
                    ));
                }
                if body.contains_key("options") && !body.contains_key("definition") {
                    return Err(invalid("options requires definition"));
                }
            }
        }
        if let Some(name) = body.get("displayName").and_then(Value::as_str) {
            if name.is_empty()
                || (kind == "workspace"
                    && (name.chars().count() > 256 || name == "Admin monitoring"))
            {
                return Err(invalid("Invalid resource displayName"));
            }
        }
        if let Some(description) = body.get("description").and_then(Value::as_str) {
            let limit = if kind == "workspace" { 4000 } else { 256 };
            if description.chars().count() > limit {
                return Err(invalid("Resource description exceeds its length limit"));
            }
        }
        if verb == "create" {
            let fields: &[&str] = if kind == "workspace" {
                &["capacityId", "domainId"]
            } else {
                &["folderId"]
            };
            for key in fields {
                if let Some(value) = body.get(*key) {
                    if !value.is_null()
                        && value
                            .as_str()
                            .is_none_or(|value| uuid::Uuid::parse_str(value).is_err())
                    {
                        return Err(invalid("Resource property must contain a GUID"));
                    }
                }
            }
        }
    }
    let body = if matches!(verb, "create" | "update") {
        let bytes = fabric_core_json::json_dumps(&Value::Object(body)).into_bytes();
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(invalid("Encoded payload exceeds allocation bound"));
        }
        bytes
    } else {
        Vec::new()
    };
    Ok(Prepared {
        kind,
        verb,
        target,
        workspace,
        body,
        query,
        force: verb == "delete" && args.get_flag("force"),
        asynchronous,
        polling,
    })
}
