// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

use fabric_cli::{
    ml_endpoint::EndpointCommand,
    parser::{Invocation, invalid_arguments_message, parse_args},
};
use fabric_sdk_workspaces::mutations::{
    ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, future::Future, pin::Pin};

const WS: &str = "11111111-2222-3333-4444-555555555555";
const MODEL: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn args(tail: &[&str]) -> Vec<String> {
    [
        "fab-rs",
        "artifact",
        "ml-endpoint",
        "score",
        "--force",
        "--workspace-id",
        WS,
        "--model-id",
        MODEL,
    ]
    .into_iter()
    .chain(tail.iter().copied())
    .map(str::to_owned)
    .collect()
}

fn command(tail: &[&str]) -> EndpointCommand {
    let Invocation::MlEndpoint(command) = parse_args(args(tail)).unwrap() else {
        panic!("ML command");
    };
    command
}

#[derive(Default)]
struct Wire {
    requests: Vec<ResourceOperation>,
    replies: VecDeque<Result<ResourceOutcome, ResourceError>>,
}
impl ResourceOperations for Wire {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.requests.push(operation);
        Box::pin(std::future::ready(
            self.replies
                .pop_front()
                .expect("unexpected dispatch or replay"),
        ))
    }
}
fn complete(data: Value) -> Result<ResourceOutcome, ResourceError> {
    Ok(ResourceOutcome::Completed {
        status: 200,
        data: Some(data),
    })
}
fn metadata(signature: Value) -> Value {
    json!({"versionName":"01","inputSignature":signature})
}
fn signature() -> Value {
    json!([{"name":"x","type":"double","required":true},{"name":"flag","type":"boolean","required":true},
        {"name":"nothing","type":"long","required":false},{"name":"text","type":"string","required":true}])
}

#[test]
fn malformed_json_and_transport_diagnostics_never_echo_inputs() {
    for body in [
        r#"{inputs:[private_payload_secret]}"#,
        r#"{"inputs":[],"inputs":[1]}"#,
        r#"{"inputs":[{"x":0,"x":1}]}"#,
    ] {
        let error = parse_args(args(&["--version", "01", "--body", body])).unwrap_err();
        let message = invalid_arguments_message(&error);
        assert!(message.contains("PowerShell"));
        assert!(!message.contains("private_payload_secret"));
        assert!(message.contains("--input"));
    }
}

#[test]
fn selectors_conflicts_and_separator_are_explicit() {
    for tail in [
        vec!["--version", "01", "--body", "{}", "--input", "file.json"],
        vec![
            "--version",
            "01",
            "--body",
            r#"{"inputs":[],"orientation":"record"}"#,
            "--orientation",
            "values",
        ],
        vec![
            "--version",
            "01",
            "--body",
            r#"{"inputs":[]}"#,
            "--orientation",
            "records",
        ],
        vec![
            "--version",
            "01",
            "--body",
            r#"{"inputs":[]}"#,
            "--format-type",
            "json",
        ],
        vec!["--version", "01", "--input-format", "csv", "--", "x=0"],
        vec!["--version", "01", "--input", "file.json", "--", "x=0"],
        vec!["--version", "01", "--", "x=0", "x=1"],
        vec!["--version", "01", "x=0"],
        vec![
            "--version",
            "01",
            "--input",
            "file.json",
            "--orientation",
            "split",
        ],
        vec!["--version", "01", "--body", r#"{"inputs":[]}"#, "--", "x=0"],
    ] {
        assert!(parse_args(args(&tail)).is_err(), "{tail:?}");
    }
}

#[tokio::test]
async fn every_native_orientation_maps_without_changing_raw_inputs_or_omissions() {
    for orientation in ["split", "values", "record", "index", "table"] {
        let mut wire = Wire {
            replies: VecDeque::from([complete(json!({"predictions":[]}))]),
            ..Wire::default()
        };
        let result = command(&[
            "--version",
            "01",
            "--body",
            r#"{"inputs":[0,false,null,{"nested":[1]}]}"#,
            "--format-type",
            "dataframe",
            "--orientation",
            orientation,
        ])
        .execute(&mut wire)
        .await
        .unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(wire.requests.len(), 1);
        assert_eq!(
            wire.requests[0].body().unwrap(),
            &json!({"inputs":[0,false,null,{"nested":[1]}],"formatType":"dataframe","orientation":orientation})
        );
    }
    let mut wire = Wire {
        replies: VecDeque::from([complete(json!({"predictions":[]}))]),
        ..Wire::default()
    };
    command(&["--default", "--body", r#"{"inputs":[0,false,null]}"#])
        .execute(&mut wire)
        .await
        .unwrap();
    assert_eq!(
        wire.requests[0].body().unwrap(),
        &json!({"inputs":[0,false,null]})
    );
    assert!(wire.requests[0].path().ends_with("/endpoint/score"));
}

#[tokio::test]
async fn default_signature_binds_the_same_exact_version_and_preserves_record_values() {
    let mut wire = Wire {
        replies: VecDeque::from([
            complete(json!({"defaultVersionName":"01"})),
            complete(metadata(signature())),
            complete(json!({"predictions":[0,false,null]})),
        ]),
        ..Wire::default()
    };
    let result = command(&[
        "--default",
        "--wait",
        "--",
        "x=0",
        "flag=false",
        "nothing=null",
        "text=literal text",
    ])
    .execute(&mut wire)
    .await
    .unwrap();
    assert_eq!(
        result["selection"],
        json!({"mode":"default","versionName":"01"})
    );
    assert_eq!(wire.requests.len(), 3);
    assert!(wire.requests[1].path().ends_with("/versions/01"));
    assert!(wire.requests[2].path().ends_with("/versions/01/score"));
    assert_eq!(
        wire.requests[2].body().unwrap(),
        &json!({"formatType":"dataframe","orientation":"record",
        "inputs":[{"x":0,"flag":false,"nothing":null,"text":"literal text"}]})
    );
}

#[tokio::test]
async fn invalid_schema_values_never_submit_inference() {
    for (schema, record) in [
        (signature(), vec!["x=0", "flag=false", "nothing=null"]),
        (
            signature(),
            vec!["x=0", "flag=false", "nothing=null", "unknown=1"],
        ),
        (
            signature(),
            vec![
                "x=9007199254740993",
                "flag=false",
                "nothing=null",
                "text=ok",
            ],
        ),
        (
            signature(),
            vec!["x=null", "flag=false", "nothing=null", "text=ok"],
        ),
        (
            json!([{"name":"x","type":"tensor","required":true}]),
            vec!["x=0"],
        ),
        (
            json!([{"name":"x","type":"double","required":true},{"name":"x","type":"double","required":true}]),
            vec!["x=0"],
        ),
        (json!([{"name":"x","type":"double"}]), vec!["x=0"]),
    ] {
        let mut tail = vec!["--version", "01", "--"];
        tail.extend(record);
        let mut wire = Wire {
            replies: VecDeque::from([complete(metadata(schema))]),
            ..Wire::default()
        };
        assert_eq!(
            command(&tail).execute(&mut wire).await.unwrap_err().code,
            "ScoreSchemaMismatch"
        );
        assert_eq!(wire.requests.len(), 1);
    }
}

#[tokio::test]
async fn files_and_values_orientation_use_signature_order_and_reject_wrong_version() {
    for (format, bytes) in [
        (
            "json",
            br#"[{"text":"","nothing":null,"flag":false,"x":0}]"#.as_slice(),
        ),
        (
            "csv",
            b"text,nothing,flag,x\r\n\"\"\"\"\"\",null,false,0\r\n".as_slice(),
        ),
    ] {
        let mut command = command(&[
            "--version",
            "01",
            "--input",
            "fixture",
            "--input-format",
            format,
            "--orientation",
            "values",
        ]);
        command.prepare_input(bytes).unwrap();
        let mut wire = Wire {
            replies: VecDeque::from([
                complete(metadata(signature())),
                Ok(ResourceOutcome::Accepted {
                    operation_id: Some("operation".into()),
                }),
            ]),
            ..Wire::default()
        };
        let result = command.execute(&mut wire).await.unwrap();
        assert_eq!(result["status"], "accepted");
        assert_eq!(
            wire.requests[1].body().unwrap(),
            &json!({"formatType":"dataframe","orientation":"values","inputs":[[0,false,null,""]]})
        );
    }
    let mut wire = Wire {
        replies: VecDeque::from([complete(
            json!({"versionName":"02","inputSignature":signature()}),
        )]),
        ..Wire::default()
    };
    assert_eq!(
        command(&["--version", "01", "--", "x=0"])
            .execute(&mut wire)
            .await
            .unwrap_err()
            .code,
        "InvalidResponse"
    );
    assert_eq!(wire.requests.len(), 1);
}

#[tokio::test]
async fn schema_mode_post_failure_keeps_uncertainty_and_is_never_replayed() {
    let error = ResourceError {
        code: "ExecutionFailed".into(),
        mutation_may_have_occurred: true,
        operation_id: Some("operation".into()),
        ..ResourceError::input()
    };
    let mut wire = Wire {
        replies: VecDeque::from([
            complete(metadata(
                json!([{"name":"x","type":"double","required":true}]),
            )),
            Err(error.clone()),
        ]),
        ..Wire::default()
    };
    assert_eq!(
        command(&["--version", "01", "--", "x=0"])
            .execute(&mut wire)
            .await
            .unwrap_err(),
        error
    );
    assert_eq!(wire.requests.len(), 2);
}

#[tokio::test]
async fn parquet_files_validate_exact_values_and_precision_before_post() {
    use arrow_array::{Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;
    for (number, valid) in [(0, true), (9007199254740993_i64, false)] {
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from(vec![number]))],
        )
        .unwrap();
        let mut bytes = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut bytes, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        let mut command = command(&[
            "--version",
            "01",
            "--input",
            "fixture.parquet",
            "--input-format",
            "parquet",
        ]);
        command.prepare_input(&bytes).unwrap();
        let mut wire = Wire {
            replies: VecDeque::from([
                complete(metadata(
                    json!([{"name":"x","type":"double","required":true}]),
                )),
                complete(json!({"predictions":[0]})),
            ]),
            ..Wire::default()
        };
        let result = command.execute(&mut wire).await;
        if valid {
            result.unwrap();
            assert_eq!(wire.requests.len(), 2);
            assert_eq!(wire.requests[1].body().unwrap()["inputs"], json!([{"x":0}]));
        } else {
            assert_eq!(result.unwrap_err().code, "ScoreSchemaMismatch");
            assert_eq!(wire.requests.len(), 1);
        }
    }
}
