// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone public consumer: query -> existing envelope -> caller-owned streams.

use fabric_cli_output::{Indent, OutputEnvelope, OutputInput, OutputStatus, Timestamp};
use fabric_cli_query::{
    QueryErrorKind, SearchOptions, join_query_args, project, search_with_options,
};
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/query/cases.json"
    ))?;
    let mut observations = Vec::new();
    for case in fixtures["cases"].as_array().ok_or("missing cases")? {
        let tokens = case["tokens"].as_array().map(|tokens| {
            tokens
                .iter()
                .map(|token| token.as_str().expect("string token"))
                .collect::<Vec<_>>()
        });
        let joined = tokens.as_ref().map(|tokens| join_query_args(tokens));
        let expression = joined.as_deref().or(case["expression"].as_str());
        let projected = match expression {
            Some(expression) => search_with_options(
                &case["data"],
                expression,
                SearchOptions {
                    deep_traversal: case["deep"].as_bool().unwrap_or(false),
                },
            ),
            None => project(&case["data"], None),
        };
        let mut input = OutputInput {
            command: Some("query".to_owned()),
            ..OutputInput::default()
        };
        let result = match projected {
            Ok(value) => {
                input.data = Some(value.clone());
                json!({"value": value})
            }
            Err(error) => {
                input.status = OutputStatus::Failure;
                input.error_code = Some(error.code().to_owned());
                input.message = Some(error.message().to_owned());
                if error.kind() == QueryErrorKind::Unsupported {
                    json!({"error": error.code(), "message": error.message(), "unsupported": true})
                } else {
                    json!({"error": error.code(), "message": error.message()})
                }
            }
        };
        let envelope = OutputEnvelope::new(
            Timestamp::parse(fixtures["timestamp"].as_str().ok_or("timestamp")?)?,
            input,
        );
        let plan = envelope.render(Indent::Four)?;
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        plan.write_to(&mut stdout, &mut stderr)?;
        observations.push(json!({
            "name": case["name"],
            "result": result,
            "exit": plan.exit_code,
            "stdout": String::from_utf8(stdout)?,
            "stderr": String::from_utf8(stderr)?,
        }));
    }
    println!("{}", serde_json::to_string(&observations)?);
    Ok(())
}
