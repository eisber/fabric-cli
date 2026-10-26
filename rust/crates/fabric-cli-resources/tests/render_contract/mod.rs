// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_cli_output::{
    OutputStream,
    text::{OutputFormat, TextError, ascii_width},
};
use fabric_cli_resources::{
    ApiOptions, Confirmation, FamilyPresentation, FamilySurface, FindOptions, LabelChange,
    MutationOptions, api, find, labels_from_bytes, mutate_label, render_family_error,
    render_family_output,
};
use fabric_sdk_access::LabelItem;
use fabric_sdk_api::{Origin, RequestBounds};
use fabric_sdk_catalog::CatalogClient;
use std::{future::Future, pin::Pin};

struct NoPrompt;
impl Confirmation for NoPrompt {
    fn confirm<'a>(
        &'a mut self,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, fabric_cli_resources::CommandError>> + Send + 'a>>
    {
        panic!("force never prompts")
    }
}

#[tokio::test]
async fn actual_family_http_and_shared_renderers_match_whole_python_handler_streams_and_order() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/access/render_cases.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let kind = case["kind"].as_str().unwrap();
        let surface = match kind {
            "acl" if case["command"] == "ls" => FamilySurface::AclList,
            "acl" => FamilySurface::Get,
            "label-local" => FamilySurface::LocalLabels,
            "find" => FamilySurface::Find {
                long: case["long"].as_bool().unwrap(),
            },
            "label-remove" => FamilySurface::Mutation,
            _ => FamilySurface::Api,
        };
        let format = if case["format"] == "text" {
            OutputFormat::Text
        } else {
            OutputFormat::Json
        };
        let mut options = FamilyPresentation::new(format, surface);
        options.terminal_width = Some(case["width"].as_u64().unwrap() as usize);
        let timestamp = Timestamp::parse("2026-01-02T03:04:05.000006Z").unwrap();
        let command = match kind {
            "find" => "find",
            "api" => "api",
            "label-local" | "label-remove" => "label",
            "error" if case["handled"] == false => "find",
            _ => "acl",
        };
        let mut wire = None;
        let result = if kind == "error" {
            let mut error = fabric_cli_resources::CommandError::from(ApiError::Http {
                status: if case["handled"] == true { 400 } else { 401 },
                code: case["code"].as_str().unwrap().into(),
            });
            error.message = case["message"].as_str().unwrap().into();
            error.code = case["code"].as_str().unwrap().into();
            error.handled = case["handled"].as_bool().unwrap();
            error.diagnostics = case["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().into())
                .collect();
            render_family_error(&error, command, timestamp, &options)
        } else {
            let value = if kind == "label-local" {
                labels_from_bytes(&serde_json::to_vec(&json!({"labels":case["labels"]})).unwrap())
                    .unwrap()
            } else {
                let (api_client, requests, _) = setup(vec![reply(
                    200,
                    &case.get("body").cloned().unwrap_or(json!({})).to_string(),
                    &[],
                )]);
                wire = Some(requests);
                match kind {
                    "acl" => read_acls(
                        &AccessClient::new(api_client),
                        &resource(case["resource"].as_str().unwrap()),
                        if case["command"] == "ls" {
                            AclRead::List {
                                long: case["long"].as_bool().unwrap(),
                                query: case["query"].as_str().map(str::to_owned),
                            }
                        } else {
                            AclRead::Get {
                                query: case["query"].as_str().map(str::to_owned),
                            }
                        },
                        Limits::default(),
                        pending(),
                    )
                    .await
                    .unwrap(),
                    "find" => find(
                        &CatalogClient::new(api_client),
                        &FindOptions {
                            search: "report".into(),
                            long: case["long"].as_bool().unwrap(),
                            ..FindOptions::default()
                        },
                        Limits::default(),
                        pending(),
                    )
                    .await
                    .unwrap(),
                    "label-remove" => mutate_label(
                        &AccessClient::new(api_client),
                        &LabelItem {
                            id: "item".into(),
                            item_type: "Notebook".into(),
                        },
                        &LabelChange::Remove,
                        Some(std::path::Path::new("configured-source.json")),
                        MutationOptions {
                            force: true,
                            ..MutationOptions::default()
                        },
                        &mut NoPrompt,
                        pending(),
                    )
                    .await
                    .unwrap(),
                    _ => api(
                        &api_client,
                        &ApiOptions {
                            origin: Origin::Fabric,
                            method: azure_core::http::Method::Get,
                            endpoint: "synthetic".into(),
                            params: None,
                            headers: None,
                            input: None,
                            query: Some(".".into()),
                            show_headers: false,
                            bounds: RequestBounds::default(),
                        },
                        NonZeroUsize::new(4096).unwrap(),
                        pending(),
                    )
                    .await
                    .unwrap(),
                }
            };
            render_family_output(&value, command, timestamp, &options, &|character: char| {
                if character == '\u{2026}' {
                    Some(1)
                } else {
                    ascii_width(character)
                }
            })
        };
        if let Some(requests) = wire {
            let requests: Vec<_> = requests
                .requests
                .lock()
                .unwrap()
                .iter()
                .map(|r| json!({"method":r.method().to_string(),"url":r.url().as_str()}))
                .collect();
            assert_eq!(
                json!(requests),
                case["expected"]["requests"],
                "{}",
                case["name"]
            );
        }
        if case["expected"]["error"].is_null() {
            let plan = result.unwrap_or_else(|e| panic!("{}: {e}", case["name"]));
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            plan.write_to(&mut stdout, &mut stderr).unwrap();
            assert_eq!(
                String::from_utf8(stdout).unwrap(),
                case["expected"]["stdout"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                String::from_utf8(stderr).unwrap(),
                case["expected"]["stderr"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                plan.exit_code,
                case["expected"]["exit"].as_u64().unwrap() as u8
            );
            let mut events: Vec<Value> = Vec::new();
            for record in plan.records {
                if record.payload.is_empty() {
                    continue;
                }
                let stream = match record.stream {
                    OutputStream::Stdout => "stdout",
                    OutputStream::Stderr => "stderr",
                };
                let payload = String::from_utf8(record.payload).unwrap();
                if let Some(last) = events.last_mut()
                    && last["stream"] == stream
                {
                    last["payload"] =
                        json!(format!("{}{}", last["payload"].as_str().unwrap(), payload));
                } else {
                    events.push(json!({"stream":stream,"payload":payload}));
                }
            }
            assert_eq!(
                json!(events),
                case["expected"]["events"],
                "{}",
                case["name"]
            );
        } else {
            let error = result.unwrap_err();
            let envelope = error
                .command_error()
                .envelope(Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), None);
            assert_eq!(
                envelope.result().message(),
                case["expected"]["error"]["message"].as_str()
            );
            assert_eq!(
                envelope.result().error_code(),
                case["expected"]["error"]["code"].as_str()
            );
        }
    }
}

#[test]
fn family_render_io_failure_and_missing_unicode_properties_are_not_silent_success() {
    let value = fabric_cli_resources::CommandValue {
        data: Some(json!([{"name":"\u{96ea}","type":"User"}])),
        ..Default::default()
    };
    let result = render_family_output(
        &value,
        "acl",
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &FamilyPresentation::new(OutputFormat::Text, FamilySurface::AclList),
        &ascii_width,
    );
    assert!(matches!(
        result,
        Err(TextError::MissingWidth(_)) | Err(TextError::AfterOutput { .. })
    ));
    struct FailWriter;
    impl std::io::Write for FailWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("synthetic output failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let plain = fabric_cli_resources::CommandValue {
        data: Some(json!([{"name":"Alice","type":"User"}])),
        ..Default::default()
    };
    let plan = render_family_output(
        &plain,
        "acl",
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &FamilyPresentation::new(OutputFormat::Text, FamilySurface::AclList),
        &ascii_width,
    )
    .unwrap();
    assert!(plan.write_to(&mut FailWriter, &mut Vec::new()).is_err());
}

#[test]
fn family_renderer_retains_prefix_on_render_failure_and_obeys_injected_newlines_without_mutating_data()
 {
    let value = fabric_cli_resources::CommandValue {
        data: Some(Value::Null),
        diagnostics: vec!["Searching...".into()],
        ..Default::default()
    };
    let options = FamilyPresentation::new(OutputFormat::Text, FamilySurface::Find { long: false });
    let error = render_family_output(
        &value,
        "find",
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &options,
        &ascii_width,
    )
    .unwrap_err();
    let partial = error
        .partial_output()
        .expect("source prefix must not disappear");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    partial.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(stderr, b"Searching...\n");
    assert!(stdout.is_empty());
    let value = fabric_cli_resources::CommandValue {
        message: Some("ACL set".into()),
        diagnostics: vec!["Adding ACL...".into()],
        ..Default::default()
    };
    let before = value.clone();
    let mut options = FamilyPresentation::new(OutputFormat::Text, FamilySurface::Mutation);
    options.presentation.newline = "\r\n";
    let plan = render_family_output(
        &value,
        "acl",
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &options,
        &ascii_width,
    )
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(stderr, b"Adding ACL...\r\n");
    assert!(stdout.ends_with(b"\r\n"));
    assert_eq!(value, before);
}
