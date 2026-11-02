// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_cli_resources::{
    CommandError, CommandValue, Confirmation, FindOutput, find_interactive,
};
use std::pin::Pin;

struct Prompts {
    answers: VecDeque<bool>,
    questions: Vec<String>,
}
impl Confirmation for Prompts {
    fn confirm<'a>(
        &'a mut self,
        question: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool, CommandError>> + Send + 'a>> {
        self.questions.push(question.into());
        let answer = self.answers.pop_front().expect("unscripted search prompt");
        Box::pin(ready(Ok(answer)))
    }
}
#[derive(Default)]
struct Output {
    data: Vec<Value>,
    diagnostics: Vec<String>,
}
impl FindOutput for Output {
    fn emit(&mut self, value: CommandValue) -> Result<(), CommandError> {
        if let Some(data) = value.data {
            self.data.push(data);
        }
        self.diagnostics.extend(value.diagnostics);
        Ok(())
    }
}

#[tokio::test]
async fn source_interactive_empty_pages_prompts_eof_and_cumulative_counts_are_observed_before_next_request()
 {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/compatibility/catalog/interactive_cases.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let replies = case["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                reply(
                    r["status"].as_u64().unwrap() as u16,
                    &r["body"].to_string(),
                    &[],
                )
            })
            .collect();
        let (api, wire, _) = setup(replies);
        let options = FindOptions {
            search: "needle".into(),
            params: case["params"].as_str().map(str::to_owned),
            ..FindOptions::default()
        };
        let mut prompts = Prompts {
            answers: case["confirmations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| a.as_bool().unwrap())
                .collect(),
            questions: Vec::new(),
        };
        let mut output = Output::default();
        let client = CatalogClient::new(api);
        let result = if case["interactive"].as_bool().unwrap_or(false) {
            find_interactive(
                &client,
                &options,
                Limits::default(),
                &mut prompts,
                &mut output,
                pending(),
            )
            .await
        } else {
            find(&client, &options, Limits::default(), pending())
                .await
                .and_then(|value| output.emit(value))
        };
        let expected = &case["expected"];
        if expected["error"].is_null() {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.code,
                expected["error"]["code"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                error.message,
                expected["error"]["message"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            output.diagnostics.extend(error.diagnostics);
        }
        assert_eq!(json!(output.data), expected["outputs"], "{}", case["name"]);
        assert_eq!(
            json!(output.diagnostics),
            expected["diagnostics"],
            "{}",
            case["name"]
        );
        assert_eq!(
            json!(prompts.questions),
            expected["questions"],
            "{}",
            case["name"]
        );
        let actual:Vec<_>=wire.requests.lock().unwrap().iter().map(|r|json!({"method":r.method().to_string(),"url":r.url().as_str(),"body":serde_json::from_slice::<Value>(&azure_core::Bytes::from(r.body())).unwrap()})).collect();
        assert_eq!(json!(actual), expected["requests"], "{}", case["name"]);
    }
}
