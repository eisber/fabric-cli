// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public job output composition, shared by tests and a probe executable.

#[path = "support.rs"]
pub(crate) mod support;

use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_jobs::{
    Confirmation, JobConfig, JobEvent, JobHost, JobRenderOptions, JobUi, NoFiles,
    command_from_matches, job_subcommand,
};
use fabric_cli_output::{
    CommandError, Timestamp,
    text::{OutputFormat, Presentation, Style, TextOptions},
};
use serde_json::{Value, json};
use std::{future::pending, sync::Arc};
use support::*;

struct Ui {
    confirmed: bool,
}

impl JobUi for Ui {
    fn emit(&self, _: JobEvent) -> Result<(), CommandError> {
        panic!("rendering adapter must not duplicate the original UI's event writes")
    }
    fn confirm(&self) -> Confirmation<'_> {
        Box::pin(async { Ok(self.confirmed) })
    }
}

/// One explicit-width fixture provider, not a product Unicode approximation.
pub fn fixture_width(character: char) -> Option<usize> {
    if character.is_ascii() {
        Some(1)
    } else if character == '\u{65e5}' {
        Some(2)
    } else {
        None
    }
}

fn color(style: Style) -> (&'static str, &'static str) {
    match style {
        Style::Plain => ("", ""),
        Style::Grey => ("<grey>", "</grey>"),
        Style::Success => ("<green>", "</green>"),
        Style::Error => ("<red>", "</red>"),
        Style::Warning => ("<yellow>", "</yellow>"),
        Style::Info => ("<blue>", "</blue>"),
        Style::Fabric => ("<fabric>", "</fabric>"),
    }
}

/// Execute a complete actual job handler and return output/request observations.
pub async fn observe(case: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut replies = Vec::new();
    for reply in case["replies"]
        .as_array()
        .ok_or("missing response script")?
    {
        if reply["accepted"] == true {
            replies.push(accepted());
        } else {
            let headers: Vec<_> = reply
                .get("headers")
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.as_str(), v.as_str().expect("fixture header")))
                        .collect()
                })
                .unwrap_or_default();
            replies.push(response(
                reply["status"].as_u64().ok_or("status")? as u16,
                &reply.get("data").map(Value::to_string).unwrap_or_default(),
                &headers,
            ));
        }
    }
    let http = ScriptedHttp::new(replies);
    let client = sdk(http.clone(), Arc::new(TestTimer::default()));
    let tenant = Element::tenant("tenant", "tenant");
    let workspace = Element::child(
        "demo",
        Some("ws".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant,
    )?;
    let item = Element::child(
        "item",
        Some("item".into()),
        NodeKind::Item(ItemType::parse("DataPipeline")?),
        workspace.clone(),
    )?;
    let lookup = Catalog::new(vec![workspace.clone(), item]);
    let ui = Ui {
        confirmed: case["confirmed"] == true,
    };
    let host = JobHost {
        sdk: &client,
        operations: None,
        lookup: &lookup,
        current: workspace,
        home: HomeContext::Unspecified,
        files: &NoFiles,
        ui: &ui,
        config: JobConfig {
            cancel_on_timeout: case
                .get("cancel_on_timeout")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        timestamp: Timestamp::parse("2026-01-02T03:04:05.000006Z")?,
    };
    let tokens = std::iter::once("job").chain(
        case["args"]
            .as_array()
            .ok_or("args")?
            .iter()
            .map(|v| v.as_str().expect("fixture token")),
    );
    let matches = job_subcommand().try_get_matches_from(tokens)?;
    let command = command_from_matches(&matches).map_err(|_| "invalid static fixture")?;
    let options = JobRenderOptions {
        format: if case["format"] == "json" {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        },
        text: TextOptions {
            terminal_width: case["width"].as_u64().map(|n| n as usize),
            ..TextOptions::default()
        },
        widths: &fixture_width,
        presentation: Presentation {
            tty: case["tty"] == true,
            color,
            newline: if case["crlf"] == true { "\r\n" } else { "\n" },
            fallback: case["fallback"] == true,
        },
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = host
        .run(&command, pending(), &options, &mut stdout, &mut stderr)
        .await?;
    let requests = http
        .requests
        .lock()
        .map_err(|_| "request lock")?
        .iter()
        .map(|request| {
            let mut uri = request
                .url()
                .path()
                .strip_prefix("/v1/")
                .expect("Fabric path")
                .to_owned();
            if let Some(query) = request.url().query() {
                uri.push('?');
                uri.push_str(query);
            }
            json!({"method":request.method().to_string().to_lowercase(),"uri":uri})
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"name":case["name"],"stdout":String::from_utf8(stdout)?,
        "stderr":String::from_utf8(stderr)?,"exit":exit,"requests":requests}),
    )
}
