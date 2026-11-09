// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_output::{
    CommandError, HiddenItems, OutputEnvelope, OutputInput, Timestamp,
    text::{self, Notice, OutputFormat, Presentation, Style, TextError, TextOptions},
};
use serde_json::{Value, json};

pub fn color(style: Style) -> (&'static str, &'static str) {
    match style {
        Style::Grey => ("<grey>", "</grey>"),
        Style::Success => ("<green>", "</green>"),
        Style::Error => ("<red>", "</red>"),
        Style::Warning => ("<yellow>", "</yellow>"),
        Style::Info => ("<blue>", "</blue>"),
        Style::Fabric => ("<fabric>", "</fabric>"),
        Style::Plain => ("", ""),
    }
}

pub fn observe(case: &Value) -> Value {
    let result = plan(case);
    match result {
        Ok(plan) => {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            plan.write_to(&mut stdout, &mut stderr).unwrap();
            json!({
                "name": case["name"], "stdout": String::from_utf8(stdout).unwrap(),
                "stderr": String::from_utf8(stderr).unwrap(), "exit": plan.exit_code,
            })
        }
        Err(error) => {
            let value = error
                .command_error()
                .envelope(Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), None)
                .to_value()
                .unwrap();
            let mut observed = json!({"name":case["name"],"error":error.to_string(),"code":value["result"]["error_code"]});
            if let Some(partial) = error.partial_output() {
                let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                partial.write_to(&mut stdout, &mut stderr).unwrap();
                observed["stdout"] = Value::String(String::from_utf8(stdout).unwrap());
                observed["stderr"] = Value::String(String::from_utf8(stderr).unwrap());
            }
            observed
        }
    }
}

fn plan(case: &Value) -> Result<text::TextPlan, TextError> {
    let input = &case["input"];
    let optional = |key: &str| input[key].as_str();
    let presentation = Presentation {
        tty: case["tty"] == true,
        color,
        newline: if case["crlf"] == true { "\r\n" } else { "\n" },
        fallback: case["fallback"] == true,
    };
    let width = |c| {
        Some(if matches!(c, '\u{540d}' | '\u{754c}') {
            2
        } else {
            1
        })
    };
    let kind = case["kind"].as_str().unwrap_or("render");
    if kind == "version" {
        return Ok(text::render_version(
            optional("version").unwrap(),
            presentation,
        ));
    }
    if kind == "help" {
        let categories = input["categories"].as_object().unwrap();
        let commands: Vec<Vec<_>> = categories
            .values()
            .map(|category| {
                category
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(name, description)| (name.as_str(), description.as_str().unwrap()))
                    .collect()
            })
            .collect();
        let categories: Vec<_> = categories
            .keys()
            .zip(&commands)
            .map(|(name, commands)| text::HelpCategory { name, commands })
            .collect();
        return text::render_help(&categories, optional("header"), presentation);
    }
    if kind == "key-values" {
        return text::render_key_values(
            &input["data"],
            text::title_key,
            text::python_string,
            presentation,
        );
    }
    if kind == "plain" || kind == "grey" || kind == "fabric" {
        let style = match kind {
            "grey" => Style::Grey,
            "fabric" => Style::Fabric,
            _ => Style::Plain,
        };
        let stream = if case["to_stderr"] == true {
            fabric_cli_output::OutputStream::Stderr
        } else {
            fabric_cli_output::OutputStream::Stdout
        };
        return Ok(text::render_line(
            optional("message").unwrap_or(""),
            style,
            stream,
            presentation,
        ));
    }
    if kind == "table" {
        let fields: Vec<_> = input["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_owned())
            .collect();
        return text::render_table(
            &input["data"],
            &fields,
            input["show_headers"] == true,
            case["width"].as_u64().unwrap_or(80) as usize,
            &width,
            presentation,
        );
    }
    if matches!(
        kind,
        "error" | "fabric-error" | "onelake-error" | "azure-error"
    ) {
        let details: Vec<_> = input["details"]
            .as_array()
            .map(|xs| {
                xs.iter()
                    .map(|x| text::ErrorDetail {
                        code: x["errorCode"].as_str(),
                        message: x["message"].as_str(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let decoration = match kind {
            "fabric-error" => text::ErrorDecoration::Fabric {
                request_id: optional("request_id"),
                details: &details,
                verbose: input["verbose"] == true,
            },
            "onelake-error" => text::ErrorDecoration::Onelake {
                timestamp: optional("timestamp"),
                request_id: optional("request_id"),
            },
            "azure-error" => text::ErrorDecoration::Azure {
                request_id: optional("request_id"),
            },
            _ => text::ErrorDecoration::Plain,
        };
        return Ok(text::render_decorated_error(
            &CommandError::new(
                optional("message").map(str::to_owned),
                optional("error_code").map(str::to_owned),
            ),
            optional("command"),
            decoration,
            presentation,
        ));
    }
    if kind != "render" {
        let message = optional("message").unwrap_or("");
        let notice = match kind {
            "done" => Notice::Done(message),
            "warning" => Notice::Warning(message),
            "info" => Notice::Info(message),
            "progress" => Notice::Progress {
                text: message,
                progress: optional("progress"),
            },
            _ => panic!("unknown fixture kind"),
        };
        return Ok(text::render_notice(
            notice,
            optional("command"),
            presentation,
        ));
    }
    let format = OutputFormat::resolve(
        case["format"].as_str().or(Some("text")),
        case["config"].as_str().or(Some("text")),
    )?;
    let hidden_data = input["hidden_data"].as_array().map(|xs| {
        HiddenItems::Strings(xs.iter().map(|x| x.as_str().unwrap().to_owned()).collect())
    });
    let envelope = OutputEnvelope::new(
        Timestamp::parse("2026-01-02T03:04:05.000006Z").unwrap(),
        OutputInput {
            command: optional("command").map(str::to_owned),
            data: input.get("data").cloned(),
            message: optional("message").map(str::to_owned),
            hidden_data,
            ..OutputInput::default()
        },
    );
    let columns: Vec<_> = input["truncate_columns"]
        .as_array()
        .map(|xs| xs.iter().map(|x| x.as_str().unwrap().to_owned()).collect())
        .unwrap_or_default();
    text::render(
        &envelope,
        format,
        &TextOptions {
            subcommand: optional("subcommand"),
            show_headers: input["show_headers"] == true,
            key_value: input["key_value"] == true,
            truncate_columns: &columns,
            terminal_width: case["width"].as_u64().map(|x| x as usize),
            key_formatter: if case["unicode_provider"] == true {
                unicode_key
            } else {
                text::title_key
            },
            value_formatter: if case["unicode_provider"] == true {
                unicode_value
            } else {
                text::python_string
            },
        },
        &width,
        presentation,
    )
}

fn unicode_key(value: &str) -> Result<String, TextError> {
    match value {
        "\u{00e9}tat_id" => Ok("\u{00c9}tat ID".to_owned()),
        _ => text::title_key(value),
    }
}

fn unicode_value(value: &Value) -> Result<String, TextError> {
    if value == &json!(["\u{00e9}", "\u{200b}", "\u{00a0}"]) {
        Ok("['\u{00e9}', '\\u200b', '\\xa0']".to_owned())
    } else {
        text::python_string(value)
    }
}
