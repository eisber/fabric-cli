// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Mounted ML endpoint contracts, independent of credentials and live services.
#![deny(unnameable_test_items)]

use fabric_cli::{
    ml_endpoint::EndpointCommand,
    parser::{Invocation, parse_args},
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, future::Future, pin::Pin};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const MODEL: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn args(leaf: &[&str]) -> Vec<String> {
    ["fab-rs", "artifact", "ml-endpoint"]
        .into_iter()
        .chain(leaf.iter().copied())
        .chain(["--workspace-id", WORKSPACE, "--model-id", MODEL])
        .map(str::to_owned)
        .collect()
}

fn command(leaf: &[&str]) -> EndpointCommand {
    let Invocation::MlEndpoint(command) = parse_args(args(leaf)).unwrap() else {
        panic!("ML endpoint root dispatch required");
    };
    command
}

#[derive(Default)]
struct Capture {
    requests: Vec<ResourceOperation>,
    replies: VecDeque<Result<ResourceOutcome, ResourceError>>,
}

impl ResourceOperations for Capture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.requests.push(operation);
        Box::pin(std::future::ready(
            self.replies.pop_front().expect("unexpected dispatch"),
        ))
    }
}

fn complete(data: Value) -> Result<ResourceOutcome, ResourceError> {
    Ok(ResourceOutcome::Completed {
        status: 200,
        data: Some(data),
    })
}

#[tokio::test]
async fn mounted_reads_keep_native_objects_and_single_page_continuation() {
    for (leaf, suffix, data) in [
        (
            vec!["settings", "get"],
            "",
            json!({"defaultVersionName":"01","future":{"empty":[],"present":false}}),
        ),
        (
            vec!["serving-version", "get", "--version", "01"],
            "/versions/01",
            json!({"versionName":"01","status":"FutureStatus","scaleRule":"FutureScaleRule","future":null}),
        ),
        (
            vec!["serving-version", "list", "--continuation-token", "next+/="],
            "/versions?continuationToken=next%2B%2F%3D",
            json!({"value":[{"versionName":"01","status":"Active"}],
                "continuationToken":"next+/=","continuationUri":"https://untrusted.invalid/never-followed","future":false}),
        ),
    ] {
        let mut executor = Capture {
            replies: VecDeque::from([complete(data.clone())]),
            ..Capture::default()
        };
        let result = command(&leaf).execute(&mut executor).await.unwrap();
        assert_eq!(
            result,
            json!({"status":"completed","httpStatus":200,"data":data})
        );
        assert_eq!(executor.requests.len(), 1);
        let request = &executor.requests[0];
        assert_eq!(request.method(), ResourceMethod::Get);
        assert_eq!(
            request.path(),
            format!("/v1/workspaces/{WORKSPACE}/mlmodels/{MODEL}/endpoint{suffix}")
        );
        assert_eq!(request.body(), None);
        assert_eq!(request.completion(), CompletionPolicy::ReturnAccepted);
    }
}

#[tokio::test]
async fn explicit_default_resolves_settings_then_exact_name_without_latest_or_alias_route() {
    let data = json!({"versionName":"01","status":"Deactivated","future":0});
    let mut executor = Capture {
        replies: VecDeque::from([
            complete(
                json!({"defaultVersionAssignmentBehavior":"StaticallyConfigured",
                "defaultVersionName":"01","defaultVersionInfo":null}),
            ),
            complete(data.clone()),
        ]),
        ..Capture::default()
    };
    let result = command(&["serving-version", "get", "--default"])
        .execute(&mut executor)
        .await
        .unwrap();
    assert_eq!(result["data"], data);
    assert_eq!(
        result["selection"],
        json!({"mode":"default","versionName":"01"})
    );
    assert_eq!(executor.requests.len(), 2);
    assert!(executor.requests[0].path().ends_with("/endpoint"));
    assert!(executor.requests[1].path().ends_with("/versions/01"));
    for settings in [
        json!({}),
        json!({"defaultVersionName": null}),
        json!({"defaultVersionName": ""}),
        json!({"defaultVersionAssignmentBehavior":"NotConfigured","defaultVersionName":"1"}),
    ] {
        let mut executor = Capture {
            replies: VecDeque::from([complete(settings)]),
            ..Capture::default()
        };
        let error = command(&["serving-version", "get", "--default"])
            .execute(&mut executor)
            .await
            .unwrap_err();
        assert_eq!(error.code, "DefaultNotConfigured");
        assert_eq!(executor.requests.len(), 1);
    }
}

#[test]
fn invalid_targets_payloads_output_and_missing_approval_fail_in_parser() {
    for leaf in [
        vec!["serving-version", "get"],
        vec!["serving-version", "get", "--default", "--version", "1"],
        vec!["serving-version", "get", "--version", "../1"],
        vec!["serving-version", "get", "--version", ""],
        vec![
            "serving-version",
            "get",
            "--version",
            "1",
            "--output_format",
            "text",
        ],
        vec![
            "settings",
            "patch",
            "--body",
            r#"{"defaultVersionName":"1"}"#,
        ],
        vec!["settings", "patch", "--force", "--body", "{}"],
        vec![
            "settings",
            "patch",
            "--force",
            "--body",
            r#"{"unknown":true}"#,
        ],
        vec![
            "serving-version",
            "patch",
            "--version",
            "1",
            "--force",
            "--body",
            r#"{"scaleRule":"unknown"}"#,
        ],
        vec!["serving-version", "activate", "--default", "--force"],
        vec!["deactivate-all"],
        vec![
            "score",
            "--default",
            "--force",
            "--body",
            r#"{"inputs":null}"#,
        ],
        vec!["settings", "get", "--timeout", "0"],
        vec!["settings", "get", "--timeout", "3601"],
    ] {
        assert!(parse_args(args(&leaf)).is_err(), "{leaf:?}");
    }
    for index in [6, 8] {
        let mut input = args(&["settings", "get"]);
        input[index] = "not-a-guid".into();
        assert!(parse_args(input).is_err());
    }
    let oversized = "a".repeat(1024 * 1024 + 1);
    assert!(
        parse_args(args(&[
            "score",
            "--default",
            "--force",
            "--body",
            &oversized
        ]))
        .is_err()
    );
}

#[tokio::test]
async fn mutations_use_native_bodies_approval_and_distinct_completion_policies() {
    for (leaf, method, suffix, body, completion) in [
        (
            vec![
                "settings",
                "patch",
                "--force",
                "--body",
                r#"{"defaultVersionName":"01"}"#,
            ],
            ResourceMethod::Patch,
            "",
            Some(json!({"defaultVersionName":"01"})),
            CompletionPolicy::ReturnAccepted,
        ),
        (
            vec![
                "serving-version",
                "patch",
                "--version",
                "01",
                "--force",
                "--body",
                r#"{"scaleRule":"AllowScaleToZero"}"#,
            ],
            ResourceMethod::Patch,
            "/versions/01",
            Some(json!({"scaleRule":"AllowScaleToZero"})),
            CompletionPolicy::ReturnAccepted,
        ),
        (
            vec![
                "serving-version",
                "activate",
                "--version",
                "01",
                "--force",
                "--wait",
            ],
            ResourceMethod::Post,
            "/versions/01/activate",
            None,
            CompletionPolicy::WaitForCompletion,
        ),
        (
            vec![
                "serving-version",
                "deactivate",
                "--version",
                "01",
                "--force",
            ],
            ResourceMethod::Post,
            "/versions/01/deactivate",
            None,
            CompletionPolicy::ReturnAccepted,
        ),
        (
            vec!["deactivate-all", "--force", "--wait"],
            ResourceMethod::Post,
            "/versions/deactivateAll",
            None,
            CompletionPolicy::WaitForCompletion,
        ),
        (
            vec![
                "score",
                "--version",
                "01",
                "--force",
                "--wait",
                "--body",
                r#"{"inputs":[false,0,null,[],{}]}"#,
            ],
            ResourceMethod::Post,
            "/versions/01/score",
            Some(json!({"inputs":[false,0,null,[],{}]})),
            CompletionPolicy::WaitForResult,
        ),
    ] {
        let reply = if method == ResourceMethod::Patch {
            complete(json!({"future":false}))
        } else {
            Ok(ResourceOutcome::Accepted {
                operation_id: Some("operation".into()),
            })
        };
        let mut executor = Capture {
            replies: VecDeque::from([reply]),
            ..Capture::default()
        };
        let result = command(&leaf).execute(&mut executor).await.unwrap();
        assert_eq!(
            result["status"],
            if method == ResourceMethod::Patch {
                "completed"
            } else {
                "accepted"
            }
        );
        assert_eq!(executor.requests.len(), 1);
        let request = &executor.requests[0];
        assert_eq!(request.method(), method);
        assert!(request.path().ends_with(&format!("/endpoint{suffix}")));
        assert_eq!(request.body(), body.as_ref());
        assert_eq!(request.completion(), completion);
    }
    let native = json!({"predictions":[false,0,null,[],{}],"future":""});
    let mut executor = Capture {
        replies: VecDeque::from([complete(native.clone())]),
        ..Capture::default()
    };
    let result = command(&[
        "score",
        "--default",
        "--force",
        "--body",
        r#"{"inputs":[]}"#,
    ])
    .execute(&mut executor)
    .await
    .unwrap();
    assert_eq!(result["data"], native);
    assert!(executor.requests[0].path().contains("/mlModels/"));
    assert!(executor.requests[0].path().ends_with("/endpoint/score"));
}

#[tokio::test]
async fn malformed_known_fields_and_unexpected_statuses_are_not_success() {
    for (leaf, data) in [
        (vec!["settings", "get"], json!({"defaultVersionName":1})),
        (vec!["settings", "get"], json!({"defaultVersionInfo":false})),
        (
            vec!["serving-version", "get", "--version", "1"],
            json!({"status":42}),
        ),
        (
            vec!["serving-version", "get", "--version", "1"],
            json!({"inputSignature":[{"required":"true"}]}),
        ),
        (vec!["serving-version", "list"], json!({"value":{}})),
        (vec!["serving-version", "list"], json!({"value":[false]})),
        (
            vec!["serving-version", "list"],
            json!({"value":[],"continuationToken":7}),
        ),
    ] {
        let mut executor = Capture {
            replies: VecDeque::from([complete(data)]),
            ..Capture::default()
        };
        assert_eq!(
            command(&leaf)
                .execute(&mut executor)
                .await
                .unwrap_err()
                .code,
            "InvalidResponse"
        );
    }
    for outcome in [
        ResourceOutcome::Completed {
            status: 204,
            data: None,
        },
        ResourceOutcome::Completed {
            status: 201,
            data: Some(json!({})),
        },
        ResourceOutcome::Accepted {
            operation_id: Some("accepted-read".into()),
        },
    ] {
        let mut executor = Capture {
            replies: VecDeque::from([Ok(outcome)]),
            ..Capture::default()
        };
        assert_eq!(
            command(&["settings", "get"])
                .execute(&mut executor)
                .await
                .unwrap_err()
                .code,
            "InvalidResponse"
        );
    }
}

#[tokio::test]
async fn original_provider_error_status_and_uncertainty_are_retained_without_replay() {
    let error = ResourceError {
        code: "Forbidden".into(),
        status: Some(403),
        raw_service_error: Some(Box::new(json!({"errorCode":"Denied","future":null}))),
        operation_id: Some("operation".into()),
        mutation_may_have_occurred: true,
        ..ResourceError::input()
    };
    let mut executor = Capture {
        replies: VecDeque::from([Err(error.clone())]),
        ..Capture::default()
    };
    assert_eq!(
        command(&["serving-version", "activate", "--version", "1", "--force"])
            .execute(&mut executor)
            .await
            .unwrap_err(),
        error
    );
    assert_eq!(executor.requests.len(), 1);
}
