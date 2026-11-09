// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure existing-session Livy input and native-result acceptance contracts.

#![deny(unnameable_test_items)]

use fabric_sdk_jobs::livy::{
    InputError, ResponseError, StatementId, StatementInput, StatementObservation, StatementVerdict,
};
use serde_json::{Value, json};

#[test]
fn livy_run_01_exact_language_matrix_and_unmodified_code() {
    for (language, kind) in [
        ("pyspark", "pyspark"),
        ("scala", "spark"),
        ("sql", "sql"),
        ("r", "sparkr"),
    ] {
        let code = " \tprint('secret')\r\n";
        let input =
            StatementInput::from_json(&json!({"code": code, "language": language})).unwrap();
        assert_eq!(input.body(), json!({"code": code, "kind": kind}));
        assert_eq!(input.language().kind(), kind);
        assert!(!format!("{input:?}").contains("secret"));
    }
}

#[test]
fn livy_run_01_pure_invalid_inputs_have_no_execution_interpretation() {
    for input in [Value::Null, json!(true), json!([]), json!("code")] {
        assert_eq!(
            StatementInput::from_json(&input),
            Err(InputError::ObjectRequired)
        );
    }
    for code in [
        Value::Null,
        json!(0),
        json!(""),
        json!(" \r\n\t"),
        json!("\u{2003}"),
    ] {
        assert_eq!(
            StatementInput::from_json(&json!({"code":code,"language":"pyspark"})),
            Err(InputError::CodeRequired)
        );
    }
    assert_eq!(
        StatementInput::from_json(&json!({"language":"pyspark"})),
        Err(InputError::CodeRequired)
    );
    for language in [
        Value::Null,
        json!(1),
        json!("python"),
        json!("spark"),
        json!("sparkr"),
        json!("SQL"),
        json!(" sql"),
    ] {
        assert_eq!(
            StatementInput::from_json(&json!({"code":"print(1)","language":language})),
            Err(InputError::LanguageRequired)
        );
    }
    assert_eq!(
        StatementInput::from_json(&json!({"code":"print(1)"})),
        Err(InputError::LanguageRequired)
    );
    for field in [
        "kind", "cell", "cellId", "notebook", "session", "oneshot", "timeout",
    ] {
        let mut input = json!({"code":"print(1)","language":"pyspark"});
        input[field] = json!("secret");
        let error = StatementInput::from_json(&input).unwrap_err();
        assert_eq!(error, InputError::UnsupportedField);
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn livy_run_02_native_id_zero_and_json_kinds_remain_distinct() {
    for id in [
        json!(0),
        json!(7),
        json!("0"),
        json!("s-7"),
        json!(u64::MAX),
    ] {
        let observation =
            StatementObservation::from_json(json!({"id":id,"state":"waiting"})).unwrap();
        assert_eq!(observation.id().native(), &id);
        assert_eq!(observation.verdict(), StatementVerdict::Pending);
    }
    assert_ne!(
        StatementId::from_json(&json!(0)).unwrap(),
        StatementId::from_json(&json!("0")).unwrap()
    );
    for id in [
        Value::Null,
        json!(""),
        json!(" "),
        json!(-1),
        json!(0.0),
        json!(true),
        json!({}),
    ] {
        assert_eq!(
            StatementObservation::from_json(json!({"id":id})),
            Err(ResponseError::StatementIdRequired)
        );
    }
    assert_eq!(
        StatementObservation::from_json(json!({"state":"waiting","sessionId":7})),
        Err(ResponseError::StatementIdRequired)
    );
}

#[test]
fn livy_run_02_http_acceptance_and_session_idle_are_not_statement_success() {
    for body in [
        json!({"id":0,"state":"waiting","output":null}),
        json!({"id":0,"state":"running"}),
    ] {
        assert_eq!(
            StatementObservation::from_json(body).unwrap().verdict(),
            StatementVerdict::Pending
        );
    }
    for body in [
        json!({"id":0,"state":"idle"}),
        json!({"id":0,"state":"new-provider-state","output":{"status":"ok"}}),
        json!({"id":0}),
        json!({"id":0,"state":null}),
        json!({"id":0,"state":"available"}),
        json!({"id":0,"state":"available","output":null}),
        json!({"id":0,"state":"available","output":{"status":"unknown"}}),
        json!({"id":0,"state":"available","output":{"status":"OK"}}),
        json!({"id":0,"state":"running","output":{"status":"ok"}}),
        json!({"id":0,"state":"running","output":{}}),
        json!({"id":0,"state":"cancelled","output":{"status":"ok"}}),
        json!({"id":0,"state":"error","output":{"status":"ok"}}),
    ] {
        assert_eq!(
            StatementObservation::from_json(body).unwrap().verdict(),
            StatementVerdict::Unverified
        );
    }
}

#[test]
fn livy_run_02_native_failure_cancel_and_complete_output_are_distinct() {
    for (state, output, verdict) in [
        (
            "available",
            json!({"status":"ok"}),
            StatementVerdict::Succeeded,
        ),
        (
            "available",
            json!({"status":"error"}),
            StatementVerdict::Failed,
        ),
        ("error", Value::Null, StatementVerdict::Failed),
        ("cancelled", Value::Null, StatementVerdict::Cancelled),
        (
            "cancelled",
            json!({"status":"error"}),
            StatementVerdict::Cancelled,
        ),
    ] {
        let body = json!({"id":0,"state":state,"output":output});
        assert_eq!(
            StatementObservation::from_json(body).unwrap().verdict(),
            verdict
        );
    }
}

#[test]
fn livy_run_02_full_media_errors_traceback_partial_and_unknown_fields_are_preserved() {
    let response = json!({
        "id":0,
        "state":"available",
        "applicationId":"application-not-statement",
        "attemptId":3,
        "replId":"repl-not-session",
        "output":{
            "status":"ok",
            "ename":"native-error-field",
            "evalue":"protected-secret",
            "traceback":["protected-secret traceback"],
            "data":{
                "text/plain":"protected-secret",
                "application/json":{"schema":[{"name":"n","type":"integer"}],"data":[[1]],"truncated":true},
                "custom/false":false,
                "custom/zero":0,
                "custom/null":null,
                "image/png":"protected-secret"
            }
        }
    });
    let observation = StatementObservation::from_json(response.clone()).unwrap();
    assert_eq!(observation.native_response(), &response);
    assert_eq!(
        observation.media().unwrap(),
        response["output"]["data"].as_object().unwrap()
    );
    assert_eq!(observation.verdict(), StatementVerdict::Succeeded);
    assert!(observation.has_partial_output());
    assert!(!format!("{observation:?}").contains("protected-secret"));
    assert!(!format!("{:?}", observation.id()).contains('0'));
}

#[test]
fn livy_run_02_missing_null_and_malformed_output_are_not_silent_empty_media() {
    for output in [
        Value::Null,
        json!({}),
        json!({"data":null}),
        json!({"data":[]}),
    ] {
        let observation = StatementObservation::from_json(json!({"id":0,"output":output})).unwrap();
        assert_eq!(observation.output(), Some(&output));
        assert_eq!(observation.media(), None);
        assert_eq!(observation.verdict(), StatementVerdict::Unverified);
    }
    let absent = StatementObservation::from_json(json!({"id":0})).unwrap();
    assert_eq!(absent.output(), None);
    assert_eq!(absent.media(), None);
}

#[test]
fn livy_run_02_partial_flags_do_not_replace_execution_verdict() {
    for output in [
        json!({"status":"ok","truncated":true}),
        json!({"status":"ok","partial":true}),
        json!({"status":"ok","data":{"application/json":{"partial":true}}}),
    ] {
        let observation =
            StatementObservation::from_json(json!({"id":0,"state":"available","output":output}))
                .unwrap();
        assert_eq!(observation.verdict(), StatementVerdict::Succeeded);
        assert!(observation.has_partial_output());
    }
    let observation = StatementObservation::from_json(
        json!({"id":0,"state":"available","output":{"status":"error","truncated":true}}),
    )
    .unwrap();
    assert_eq!(observation.verdict(), StatementVerdict::Failed);
    assert!(observation.has_partial_output());
    for flag in [json!(false), json!("true"), Value::Null] {
        let observation = StatementObservation::from_json(
            json!({"id":0,"state":"available","output":{"status":"ok","truncated":flag}}),
        )
        .unwrap();
        assert!(!observation.has_partial_output());
    }
}
