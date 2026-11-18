// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resource command parsing and local payload preparation.

use crate::{
    console::{ConsoleConfirmation, ConsoleProgress},
    definition,
};
use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_cli_command_contracts::{CommandDiagnostics, CommandError};
use fabric_core::{
    Clock, CoreError, Endpoint, HttpExecutor, MAX_REQUEST_BYTES, OperationContext,
    diagnostics::DiagnosticStage,
};
use fabric_resources::Selector;
use serde_json::{Map, Value};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

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
    pub destination: Option<PathBuf>,
    pub overwrite: bool,
    pub format: Option<String>,
    pub source: bool,
}

fn invalid(message: &str) -> CommandError {
    CommandError::new("InvalidInput", message)
}

pub(crate) fn command(kind: &'static str) -> Command {
    let mut parent = Command::new(kind)
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .arg(crate::invocation::help_arg())
        .subcommand_required(true);
    for verb in ["list", "get", "create", "update", "delete"] {
        let mut leaf = Command::new(verb)
            .disable_help_flag(true)
            .arg(crate::invocation::help_arg())
            .arg(Arg::new("query").long("query").help(
                "JMESPath on resource data; list single-field queries keep one object per row",
            ));
        if kind == "item" {
            leaf = leaf.arg(Arg::new("workspace").long("workspace").required(true));
        }
        if verb != "list" {
            leaf = leaf.arg(Arg::new("target").required(verb != "create"));
        }
        let sourced = verb == "update" || (verb == "create" && kind == "item");
        if verb == "create" || verb == "update" {
            let mut arg = Arg::new("payload").long("payload");
            arg = if verb == "update" {
                arg.required_unless_present("import-from")
            } else {
                arg
            };
            leaf = leaf.arg(arg);
        }
        if sourced {
            leaf = leaf.arg(
                Arg::new("import-from")
                    .long("import-from")
                    .conflicts_with("payload")
                    .help("Import the definition from this local directory"),
            );
        }
        if kind == "workspace" && verb == "update" {
            leaf = leaf.arg(
                Arg::new("allow-pairing-by-name")
                    .long("allow-pairing-by-name")
                    .action(ArgAction::SetTrue)
                    .requires("import-from")
                    .conflicts_with("payload")
                    .help("Pair imported items without logical IDs by name and type"),
            );
        }
        if verb == "get" {
            leaf = leaf
                .arg(
                    Arg::new("export-to")
                        .long("export-to")
                        .help("Export the definition into this existing local directory"),
                )
                .arg(
                    Arg::new("overwrite")
                        .long("overwrite")
                        .action(ArgAction::SetTrue)
                        .requires("export-to"),
                );
        }
        let file_flag = if verb == "get" {
            "export-to"
        } else {
            "import-from"
        };
        if kind == "item" && (verb == "get" || sourced) {
            let mut format = Arg::new("format").long("format").requires(file_flag);
            if sourced {
                format = format.conflicts_with("payload");
            }
            leaf = leaf.arg(format);
        }
        if kind == "item" && verb == "create" {
            leaf = leaf.arg(
                Arg::new("type")
                    .long("type")
                    .requires("import-from")
                    .conflicts_with("payload"),
            );
        }
        if verb == "delete" {
            leaf = leaf.arg(Arg::new("force").long("force").action(ArgAction::SetTrue));
        }
        if verb == "update" {
            leaf = leaf.arg(
                Arg::new("force")
                    .long("force")
                    .action(ArgAction::SetTrue)
                    .requires("import-from")
                    .conflicts_with("payload"),
            );
        }
        if matches!(verb, "create" | "update" | "delete") {
            leaf = leaf
                .arg(Arg::new("async").long("async").action(ArgAction::SetTrue))
                .arg(Arg::new("polling-config").long("polling-config"));
        }
        if verb == "get" {
            leaf = leaf
                .arg(
                    Arg::new("async")
                        .long("async")
                        .action(ArgAction::SetTrue)
                        .requires("export-to"),
                )
                .arg(
                    Arg::new("polling-config")
                        .long("polling-config")
                        .requires("export-to"),
                );
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

pub(crate) fn prepare(
    kind: &'static str,
    matches: &ArgMatches,
    home: &dyn Fn() -> Option<PathBuf>,
) -> Result<Prepared, CommandError> {
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
    finish_prepare(kind, args, verb, target, home)
}

pub(crate) async fn execute(
    prepared: Prepared,
    client: &dyn HttpExecutor,
    clock: Arc<dyn Clock>,
    service: &Endpoint,
    context: &OperationContext,
    observer: &Arc<ConsoleProgress>,
    confirmer: &ConsoleConfirmation,
) -> Result<Value, CommandError> {
    let workspace_id = match &prepared.workspace {
        Some(selector) => Some(
            fabric_resources::resolve(
                client,
                clock.as_ref(),
                service,
                context,
                "workspaces",
                selector,
            )
            .await?,
        ),
        None => None,
    };
    let collection = match &workspace_id {
        Some(id) => format!("workspaces/{id}/items"),
        None => "workspaces".to_owned(),
    };
    let target_id = match (&prepared.target, prepared.verb) {
        (Some(selector), "get" | "update" | "delete") => Some(
            fabric_resources::resolve(
                client,
                clock.as_ref(),
                service,
                context,
                &collection,
                selector,
            )
            .await?,
        ),
        _ => None,
    };
    let export = prepared.destination.is_some();
    let import = prepared.source && prepared.verb == "update";
    if (prepared.verb == "delete" || import) && !prepared.force {
        context.remaining_at(clock.now())?;
        let id = target_id
            .as_deref()
            .ok_or_else(|| invalid("Missing resource target"))?;
        let scope = workspace_id
            .as_ref()
            .map(|id| format!(" in workspace {id}"))
            .unwrap_or_default();
        let prompt = match (prepared.verb, prepared.kind) {
            ("delete", kind) => format!("Delete {kind} {id}{scope}? [Y/n] "),
            (_, "item") => format!("Replace the definition of item {id}{scope}? [Y/n] "),
            _ => {
                format!("Import definitions into workspace {id}, replacing matching items? [Y/n] ")
            }
        };
        confirmer.confirm(prompt, clock.clone(), context).await?;
        context.remaining_at(clock.now())?;
    }
    if prepared.verb == "list" {
        let data =
            fabric_resources::list(client, clock.as_ref(), service, context, &collection).await?;
        return project(data, prepared.query.as_deref(), true, observer).await;
    }
    let endpoint = match &target_id {
        Some(id) => format!("{collection}/{id}"),
        None => collection.clone(),
    };
    let mut folder = None;
    let mut body = prepared.body;
    let (method, endpoint) = match (prepared.verb, prepared.kind) {
        ("get", "item") if export => {
            folder = Some(item_folder(client, service, context, &endpoint).await?);
            let format = prepared
                .format
                .as_deref()
                .map(|format| format!("?format={format}"))
                .unwrap_or_default();
            ("POST", format!("{endpoint}/getDefinition{format}"))
        }
        ("get", _) if export => {
            body = br#"{"mode":"All"}"#.to_vec();
            (
                "POST",
                format!("{endpoint}/items/bulkExportDefinitions?beta=true"),
            )
        }
        ("update", "item") if import => ("POST", format!("{endpoint}/updateDefinition")),
        ("update", _) if import => ("POST", format!("{endpoint}/items/bulkImportDefinitions")),
        ("get", _) => ("GET", endpoint),
        ("create", _) => ("POST", endpoint),
        ("update", _) => ("PATCH", endpoint),
        ("delete", _) => ("DELETE", endpoint),
        _ => return Err(invalid("Invalid resource action")),
    };
    let (mut data, accepted) = send(
        client,
        clock.as_ref(),
        service,
        context,
        observer,
        Request {
            method,
            endpoint: &endpoint,
            body,
            asynchronous: prepared.asynchronous,
            polling: prepared.polling,
            object: prepared.verb == "get",
            allow_empty: import || prepared.verb == "delete",
        },
    )
    .await?;
    if let Some(destination) = prepared.destination.as_deref() {
        if accepted {
            if let Value::Object(envelope) = &mut data {
                envelope.insert("saved".into(), Value::Bool(false));
            }
        } else {
            data = save(
                destination.to_path_buf(),
                folder,
                data,
                prepared.overwrite,
                context,
                clock.clone(),
            )
            .await
            .map_err(|error| attach_recovery(observer, error))?;
        }
    } else if import && prepared.kind == "workspace" && !accepted {
        check_import_result(&data).map_err(|error| attach_recovery(observer, error))?;
    }
    let data = project(data, prepared.query.as_deref(), false, observer)
        .await
        .map_err(|error| attach_recovery(observer, error))?;
    if !prepared.asynchronous && observer.recovery().is_some() {
        observer
            .finish_progress()
            .await
            .map_err(|error| attach_recovery(observer, error.into()))?;
    }
    Ok(data)
}

async fn project(
    data: Value,
    query: Option<&str>,
    list: bool,
    observer: &ConsoleProgress,
) -> Result<Value, CommandError> {
    let started = observer.start(DiagnosticStage::Projection).await?;
    observer
        .finish(
            DiagnosticStage::Projection,
            started,
            apply_query(data, query, list),
        )
        .await
}

fn apply_query(data: Value, query: Option<&str>, list: bool) -> Result<Value, CommandError> {
    match query.filter(|query| !query.is_empty()) {
        Some(query) => {
            let result = if list {
                fabric_cli_query::search_list(&data, query)
            } else {
                fabric_cli_query::search(&data, query)
            };
            result.map_err(|_| invalid("Invalid JMESPath query"))
        }
        None => Ok(data),
    }
}

async fn item_folder(
    client: &dyn HttpExecutor,
    service: &Endpoint,
    context: &OperationContext,
    endpoint: &str,
) -> Result<String, CommandError> {
    let response =
        fabric_resources::request(client, service, context, "GET", endpoint, Vec::new()).await?;
    let item = fabric_core_json::decode(&response.body)
        .map_err(|_| CommandError::new("InvalidJson", "Malformed resource response JSON"))?;
    let field = |key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| CommandError::new("Protocol", "Item response lacks displayName or type"))
    };
    let folder = format!("{}.{}", field("displayName")?, field("type")?);
    definition::validate_segment(&folder)?;
    Ok(folder)
}

async fn save(
    destination: PathBuf,
    folder: Option<String>,
    data: Value,
    overwrite: bool,
    context: &OperationContext,
    clock: Arc<dyn Clock>,
) -> Result<Value, CommandError> {
    // The original request deadline does not bound a completed LRO.
    let context = OperationContext::with_deadline(None, context.cancellation.child_token());
    let _cancel_on_drop = context.cancellation.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        save_sync(
            &destination,
            folder.as_deref(),
            &data,
            overwrite,
            &context,
            clock.as_ref(),
        )
    })
    .await
    .map_err(|_| CommandError::new("FileSystem", "Definition export worker failed"))?
}

fn save_sync(
    destination: &Path,
    folder: Option<&str>,
    data: &Value,
    overwrite: bool,
    context: &OperationContext,
    clock: &dyn Clock,
) -> Result<Value, CommandError> {
    let check = || {
        context
            .remaining_at(clock.now())
            .map(|_| ())
            .map_err(|error| {
                let mut error: CommandError = error.into();
                error
                    .message
                    .push_str("; previously saved files are not rolled back");
                error
            })
    };
    check()?;
    let (files, prefix, root) = match folder {
        Some(folder) => {
            let parts = data
                .get("definition")
                .and_then(|definition| definition.get("parts"))
                .ok_or_else(|| CommandError::new("Protocol", "Definition response lacks parts"))?;
            (
                definition::decode_parts(parts, false)?,
                vec![folder],
                destination.join(folder),
            )
        }
        None => {
            let parts = data.get("definitionParts").ok_or_else(|| {
                CommandError::new("Protocol", "Bulk export response lacks definitionParts")
            })?;
            (
                definition::decode_parts(parts, true)?,
                Vec::new(),
                destination.to_path_buf(),
            )
        }
    };
    let written = definition::write_tree(destination, &prefix, &files, overwrite, &check)?;
    let mut result = Map::new();
    result.insert("saved".into(), Value::Bool(true));
    result.insert("path".into(), Value::String(root.display().to_string()));
    result.insert(
        "files".into(),
        Value::Array(written.into_iter().map(Value::String).collect()),
    );
    Ok(Value::Object(result))
}

fn check_import_result(data: &Value) -> Result<(), CommandError> {
    let details = data
        .get("importItemDefinitionsDetails")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CommandError::new(
                "Protocol",
                "Bulk import returned no item results; remote changes may have been applied",
            )
        })?;
    if details.is_empty() {
        return Err(CommandError::new(
            "Protocol",
            "Bulk import returned no item results; remote changes may have been applied",
        ));
    }
    let mut failures = Vec::new();
    let mut failure_count = 0usize;
    for detail in details {
        let status = detail
            .get("operationStatus")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CommandError::new(
                    "Protocol",
                    "Bulk import returned an item without a status; remote changes may have been applied",
                )
            })?;
        if status.eq_ignore_ascii_case("Succeeded") {
            continue;
        }
        if !status.eq_ignore_ascii_case("Failed")
            && !status.eq_ignore_ascii_case("SucceededDespiteFailures")
        {
            return Err(CommandError::new(
                "Protocol",
                "Bulk import returned an unknown item status; remote changes may have been applied",
            ));
        }
        failure_count += 1;
        if failures.len() < 3 {
            let name = detail
                .get("itemDisplayName")
                .and_then(Value::as_str)
                .and_then(fabric_core::sanitize_service_message)
                .map(|name| name.chars().take(80).collect::<String>())
                .unwrap_or_else(|| "unnamed item".to_owned());
            failures.push(format!("{name}: {status}"));
        }
    }
    if failure_count != 0 {
        return Err(CoreError::service_failure(
            format!(
                "{failure_count} item definition(s) failed or partially succeeded; remote changes may have been applied: {}",
                failures.join("; ")
            ),
            None,
        )
        .into());
    }
    Ok(())
}

struct Request<'a> {
    method: &'static str,
    endpoint: &'a str,
    body: Vec<u8>,
    asynchronous: bool,
    polling: fabric_sdk_api::LroConfig,
    object: bool,
    allow_empty: bool,
}

/// Send one request, reconcile its LRO unless asynchronous, and return `(data, accepted)`.
async fn send(
    client: &dyn HttpExecutor,
    clock: &dyn Clock,
    service: &Endpoint,
    context: &OperationContext,
    observer: &Arc<ConsoleProgress>,
    request: Request<'_>,
) -> Result<(Value, bool), CommandError> {
    let initial = fabric_resources::request(
        client,
        service,
        context,
        request.method,
        request.endpoint,
        request.body,
    )
    .await?;
    let recovery = match fabric_core::operation_handle_with_endpoint(
        service,
        initial.status,
        &initial.headers,
    ) {
        Ok(handle) => handle.map(|handle| handle.recovery()),
        Err(error) => error.recovery,
    };
    observer.set_recovery(recovery.clone());
    let attach = |error| attach_recovery(observer, error);
    let response = if request.asynchronous {
        initial
    } else {
        fabric_cli_api::validate_response_body(&initial.body).map_err(attach)?;
        let reconciliation = fabric_sdk_api::complete_observed_with_endpoint(
            client,
            clock,
            service,
            initial,
            context.cancellation.clone(),
            request.polling,
            observer.as_ref(),
        );
        tokio::select! {
            biased;
            error = observer.progress_failure() => return Err(attach(error.into())),
            result = reconciliation => result.map_err(|error| attach(error.into()))?,
        }
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
    let body = if empty
        && (response.status == 202
            || request.allow_empty
            || (response.status == 204 && !request.object))
    {
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
        if let Some(recovery) = recovery.as_ref()
            && let Ok(value) = serde_json::to_value(recovery)
        {
            accepted.insert("recovery".into(), value);
        }
        return Ok((Value::Object(accepted), true));
    }
    if request.object && !body.is_object() {
        return Err(attach(CommandError::new(
            "Protocol",
            "Resource response must be an object",
        )));
    }
    Ok((body, false))
}

fn attach_recovery(observer: &ConsoleProgress, mut error: CommandError) -> CommandError {
    if error.recovery.is_none() {
        error.recovery = observer
            .recovery()
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok())
            .map(Box::new);
    }
    error
}

fn finish_prepare(
    kind: &'static str,
    args: &ArgMatches,
    verb: &'static str,
    target: Option<Selector>,
    home: &dyn Fn() -> Option<PathBuf>,
) -> Result<Prepared, CommandError> {
    let text = |key| args.get_one::<String>(key).map(String::as_str);
    let expand = |raw: &str| definition::expand_home(raw, home);
    let workspace = if kind == "item" {
        text("workspace").map(Selector::parse).transpose()?
    } else {
        None
    };
    let query = text("query").map(str::to_owned);
    if let Some(value) = query.as_deref().filter(|value| !value.is_empty())
        && (value.len() > 16 * 1024 || fabric_cli_query::validate(value).is_err())
    {
        return Err(invalid("Invalid or oversized JMESPath query"));
    }
    let lro = matches!(verb, "get" | "create" | "update" | "delete");
    let asynchronous = lro && args.get_flag("async");
    let polling_text = if lro { text("polling-config") } else { None };
    if asynchronous && polling_text.is_some() {
        return Err(invalid("--async cannot be combined with --polling-config"));
    }
    let polling = fabric_cli_api::parse_polling_config(polling_text)?;
    let destination = if verb == "get" {
        text("export-to").map(expand).transpose()?
    } else {
        None
    };
    if let Some(path) = &destination {
        definition::require_directory(path, "--export-to")?;
    }
    let sourced = verb == "update" || (verb == "create" && kind == "item");
    let source = if sourced {
        text("import-from").map(expand).transpose()?
    } else {
        None
    };
    let format = if kind == "item" && (verb == "get" || sourced) {
        text("format").map(str::to_owned)
    } else {
        None
    };
    if let Some(value) = &format
        && (value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    {
        return Err(invalid("Invalid definition --format"));
    }
    let overwrite = verb == "get" && args.get_flag("overwrite");
    let force = matches!(verb, "delete" | "update") && args.get_flag("force");
    if let Some(root) = &source {
        definition::require_directory(root, "--import-from")?;
        let body = source_body(kind, verb, &target, args, root, format.as_deref())?;
        return Ok(Prepared {
            kind,
            verb,
            target,
            workspace,
            body,
            query,
            force,
            asynchronous,
            polling,
            destination,
            overwrite,
            format,
            source: true,
        });
    }
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
        if let Some(name) = body.get("displayName").and_then(Value::as_str)
            && (name.is_empty()
                || (kind == "workspace"
                    && (name.chars().count() > 256 || name == "Admin monitoring")))
        {
            return Err(invalid("Invalid resource displayName"));
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
                if let Some(value) = body.get(*key)
                    && !value.is_null()
                    && value
                        .as_str()
                        .is_none_or(|value| uuid::Uuid::parse_str(value).is_err())
                {
                    return Err(invalid("Resource property must contain a GUID"));
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
        force,
        asynchronous,
        polling,
        destination,
        overwrite,
        format,
        source: false,
    })
}

/// Build an import request body from a local definition directory.
fn source_body(
    kind: &str,
    verb: &str,
    target: &Option<Selector>,
    args: &ArgMatches,
    root: &Path,
    format: Option<&str>,
) -> Result<Vec<u8>, CommandError> {
    let mut parts = definition::read_tree(root, kind == "item")?;
    let mut body = Map::new();
    if kind == "workspace" {
        for part in &mut parts {
            if let Some(Value::String(path)) = part.get_mut("path") {
                path.insert(0, '/');
            }
        }
        body.insert("definitionParts".into(), Value::Array(parts));
        if args.get_flag("allow-pairing-by-name") {
            body.insert(
                "options".into(),
                serde_json::json!({"allowPairingByName": true}),
            );
        }
    } else {
        if verb == "create" {
            let Some(Selector::Name(name)) = target else {
                return Err(invalid("Creation requires displayName"));
            };
            let declared = args.get_one::<String>("type").cloned();
            let platform = definition::platform_type(&parts)?;
            let kind = match (declared, platform) {
                (Some(declared), Some(platform)) if declared != platform => {
                    return Err(invalid("--type does not match the .platform item type"));
                }
                (Some(kind), _) | (None, Some(kind)) if !kind.is_empty() => kind,
                _ => return Err(invalid("Item creation requires --type or a .platform file")),
            };
            body.insert("displayName".into(), Value::String(name.clone()));
            body.insert("type".into(), Value::String(kind));
        }
        let mut definition = Map::new();
        if let Some(format) = format {
            definition.insert("format".into(), Value::String(format.to_owned()));
        }
        definition.insert("parts".into(), Value::Array(parts));
        body.insert("definition".into(), Value::Object(definition));
    }
    let bytes = fabric_core_json::json_dumps(&Value::Object(body)).into_bytes();
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(invalid("Definition exceeds request allocation bound"));
    }
    Ok(bytes)
}
