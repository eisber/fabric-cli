// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_output::{
    CommandError, HiddenItems, Indent, OutputEnvelope, OutputInput, OutputStatus, RenderPlan,
    Timestamp,
};
use serde_json::Value;

pub fn plan(case: &Value, timestamp: &str) -> RenderPlan {
    let input = &case["input"];
    let optional = |key: &str| input[key].as_str().map(str::to_owned);
    let timestamp = Timestamp::parse(timestamp).unwrap();
    let indent = match case["indent"].as_u64().unwrap_or(4) {
        0 => Indent::Zero,
        2 => Indent::Two,
        4 => Indent::Four,
        _ => panic!("unsupported fixture indentation"),
    };
    match case["kind"].as_str().unwrap_or("envelope") {
        "invalid-json" => CommandError::invalid_json()
            .envelope(timestamp, None)
            .render(indent)
            .unwrap(),
        "error" | "decorated" => CommandError::new(optional("message"), optional("error_code"))
            .envelope(timestamp, optional("command"))
            .render(indent)
            .unwrap(),
        "unexpected" => CommandError::unexpected(optional("message"))
            .envelope(timestamp, None)
            .render(indent)
            .unwrap(),
        "cancelled" => CommandError::cancelled(timestamp, indent).unwrap(),
        "envelope" | "success" => {
            let hidden_data = input["hidden_data"].as_array().map(|values| {
                let values = values
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned())
                    .collect();
                if input["hidden_kind"] == "enum" {
                    HiddenItems::EnumValues(values)
                } else {
                    HiddenItems::Strings(values)
                }
            });
            OutputEnvelope::new(
                timestamp,
                OutputInput {
                    command: optional("command"),
                    status: if input["status"] == "Failure" {
                        OutputStatus::Failure
                    } else {
                        OutputStatus::Success
                    },
                    message: optional("message"),
                    error_code: optional("error_code"),
                    data: input.get("data").cloned(),
                    hidden_data,
                },
            )
            .render(indent)
            .unwrap()
        }
        _ => panic!("unsupported fixture kind"),
    }
}
