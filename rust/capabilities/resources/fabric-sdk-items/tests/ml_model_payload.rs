// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline external-consumer contracts for native MLModel parameter/result data.

#![deny(unnameable_test_items)]

use fabric_sdk_items::ml_models::{
    CreateMlModelRequest, DefaultVersionAssignmentBehavior, FormatType, Orientation, ScaleRule,
    ScoreDataRequest, ScoreDataResponse, ScoreOutcome, SensitivityLabelApplyStrategy,
    SensitivityLabelSettings, UpdateEndpointRequest, UpdateEndpointVersionRequest,
    UpdateMlModelRequest,
};
use fabric_sdk_workspaces::mutations::{ResourceError, ResourceOutcome};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const GUID: &str = "01234567-89ab-cdef-0123-456789abcdef";

fn reject<T: DeserializeOwned>(source: &str) {
    assert!(serde_json::from_str::<T>(source).is_err(), "{source}");
}

fn assert_local_error(error: &ResourceError) {
    assert_eq!(error.code, "InvalidInput");
    assert_eq!(error.status, None);
    assert_eq!(error.operation_id, None);
    assert!(!error.mutation_may_have_occurred);
    assert!(error.service_error.is_none());
    assert!(error.raw_service_error.is_none());
}

fn native_values() -> Vec<Value> {
    vec![
        Value::Null,
        json!(false),
        json!(0),
        json!(-1),
        json!(0.25),
        json!(u64::MAX),
        json!(""),
        json!("模型 🧪"),
        json!([]),
        json!([null, false, 0, "nested", [1, 2]]),
        json!({}),
        json!({"named": false, "nested": {"empty": [], "zero": 0}}),
    ]
}

#[test]
fn create_uses_only_exact_native_camel_case_fields() {
    let request = CreateMlModelRequest {
        display_name: "  模型 name  ".into(),
        description: Some("description".into()),
        folder_id: Some(Some(GUID.into())),
        sensitivity_label_settings: Some(SensitivityLabelSettings {
            label_id: GUID.into(),
            sensitivity_label_apply_strategy: Some(SensitivityLabelApplyStrategy::Ignore),
        }),
    };
    request.validate().unwrap();
    let expected = json!({
        "displayName": "  模型 name  ",
        "description": "description",
        "folderId": GUID,
        "sensitivityLabelSettings": {
            "labelId": GUID,
            "sensitivityLabelApplyStrategy": "Ignore"
        }
    });
    assert_eq!(serde_json::to_value(&request).unwrap(), expected);
    assert_eq!(
        serde_json::from_value::<CreateMlModelRequest>(expected).unwrap(),
        request
    );
}

#[test]
fn omitted_create_and_label_options_are_not_defaulted() {
    let request: CreateMlModelRequest =
        serde_json::from_value(json!({"displayName": "model"})).unwrap();
    request.validate().unwrap();
    assert_eq!(request.description, None);
    assert_eq!(request.folder_id, None);
    assert_eq!(request.sensitivity_label_settings, None);
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"displayName": "model"})
    );

    let settings: SensitivityLabelSettings =
        serde_json::from_value(json!({"labelId": GUID})).unwrap();
    settings.validate().unwrap();
    assert_eq!(settings.sensitivity_label_apply_strategy, None);
    assert_eq!(
        serde_json::to_value(settings).unwrap(),
        json!({"labelId": GUID})
    );
    for strategy in [
        SensitivityLabelApplyStrategy::ApplyOrFail,
        SensitivityLabelApplyStrategy::Ignore,
    ] {
        let label = SensitivityLabelSettings {
            label_id: GUID.into(),
            sensitivity_label_apply_strategy: Some(strategy),
        };
        assert_eq!(
            serde_json::from_value::<SensitivityLabelSettings>(
                serde_json::to_value(&label).unwrap()
            )
            .unwrap(),
            label
        );
    }
}

#[test]
fn create_requires_a_nonblank_name_without_normalizing_it() {
    for source in [
        r#"{}"#,
        r#"{"displayName":null}"#,
        r#"{"displayName":false}"#,
    ] {
        reject::<CreateMlModelRequest>(source);
    }
    for name in ["", " ", "\n\t", "\u{2003}"] {
        let request: CreateMlModelRequest =
            serde_json::from_value(json!({"displayName": name})).unwrap();
        assert_local_error(&request.validate().unwrap_err());
    }
}

#[test]
fn create_folder_preserves_omission_explicit_null_and_supplied_guid() {
    for (folder_id, expected) in [
        (None, json!({"displayName": "model"})),
        (
            Some(None),
            json!({"displayName": "model", "folderId": null}),
        ),
        (
            Some(Some(GUID.to_owned())),
            json!({"displayName": "model", "folderId": GUID}),
        ),
    ] {
        let request = CreateMlModelRequest {
            display_name: "model".into(),
            description: None,
            folder_id,
            sensitivity_label_settings: None,
        };
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), expected);
        let decoded: CreateMlModelRequest = serde_json::from_value(expected).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, request);
    }
    for source in [
        r#"{"displayName":"model","folderId":false}"#,
        r#"{"displayName":"model","folderId":0}"#,
        r#"{"displayName":"model","folderId":[]}"#,
        r#"{"displayName":"model","folderId":{}}"#,
    ] {
        reject::<CreateMlModelRequest>(source);
    }
}

#[test]
fn description_limits_count_characters_not_utf8_bytes() {
    for character in ["a", "界", "🧪"] {
        for count in [0, 256, 257] {
            let description = character.repeat(count);
            let create: CreateMlModelRequest = serde_json::from_value(json!({
                "displayName": "model",
                "description": description
            }))
            .unwrap();
            let update = UpdateMlModelRequest { description };
            if count <= 256 {
                create.validate().unwrap();
                update.validate().unwrap();
            } else {
                assert_local_error(&create.validate().unwrap_err());
                assert_local_error(&update.validate().unwrap_err());
            }
        }
    }
}

#[test]
fn folder_and_label_ids_require_hyphenated_guids() {
    for id in [
        "",
        "not-a-guid",
        "0123456789abcdef0123456789abcdef",
        "{01234567-89ab-cdef-0123-456789abcdef}",
        "01234567_89ab-cdef-0123-456789abcdef",
        "g1234567-89ab-cdef-0123-456789abcdef",
        "01234567-89ab-cdef-0123-456789abcde界",
    ] {
        let folder: CreateMlModelRequest = serde_json::from_value(json!({
            "displayName": "model",
            "folderId": id
        }))
        .unwrap();
        assert_local_error(&folder.validate().unwrap_err());
        let label = SensitivityLabelSettings {
            label_id: id.into(),
            sensitivity_label_apply_strategy: None,
        };
        assert_local_error(&label.validate().unwrap_err());
        let create = CreateMlModelRequest {
            display_name: "model".into(),
            description: None,
            folder_id: None,
            sensitivity_label_settings: Some(label),
        };
        assert_local_error(&create.validate().unwrap_err());
    }
    for id in [GUID.to_owned(), GUID.to_uppercase()] {
        let request: CreateMlModelRequest = serde_json::from_value(json!({
            "displayName": "model",
            "folderId": id,
            "sensitivityLabelSettings": {"labelId": id}
        }))
        .unwrap();
        request.validate().unwrap();
    }
}

#[test]
fn label_schema_rejects_fabio_spelling_unknown_properties_and_nulls() {
    for source in [
        r#"{}"#,
        r#"{"sensitivityLabelId":"01234567-89ab-cdef-0123-456789abcdef"}"#,
        r#"{"labelId":null}"#,
        r#"{"labelId":"01234567-89ab-cdef-0123-456789abcdef","unknown":false}"#,
        r#"{"labelId":"01234567-89ab-cdef-0123-456789abcdef","sensitivityLabelApplyStrategy":null}"#,
        r#"{"labelId":"01234567-89ab-cdef-0123-456789abcdef","sensitivityLabelApplyStrategy":"applyOrFail"}"#,
    ] {
        reject::<SensitivityLabelSettings>(source);
    }
    reject::<CreateMlModelRequest>(
        r#"{"displayName":"model","sensitivityLabelSettings":{"sensitivityLabelId":"01234567-89ab-cdef-0123-456789abcdef"}}"#,
    );
}

#[test]
fn update_item_is_required_description_only_including_empty_string() {
    let request: UpdateMlModelRequest = serde_json::from_value(json!({"description": ""})).unwrap();
    request.validate().unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"description": ""})
    );
    for source in [
        r#"{}"#,
        r#"{"description":null}"#,
        r#"{"description":false}"#,
        r#"{"displayName":"rename"}"#,
        r#"{"description":"","displayName":"rename"}"#,
        r#"{"description":"","definition":{"parts":[]}}"#,
        r#"{"description":"","unknown":0}"#,
    ] {
        reject::<UpdateMlModelRequest>(source);
    }
}

#[test]
fn all_request_schemas_reject_unknown_fields_and_execution_wrappers() {
    for source in [
        r#"{"displayName":"model","definition":{"parts":[]}}"#,
        r#"{"displayName":"model","type":"MLModel"}"#,
        r#"{"displayName":"model","unknown":null}"#,
    ] {
        reject::<CreateMlModelRequest>(source);
    }
    reject::<UpdateEndpointRequest>(r#"{"defaultVersionName":"v1","defaultVersionInfo":{}}"#);
    reject::<UpdateEndpointVersionRequest>(r#"{"scaleRule":"AlwaysOn","status":"Active"}"#);
    for source in [
        r#"{"inputs":[],"unknown":null}"#,
        r#"{"inputs":[],"parameters":{}}"#,
        r#"{"inputs":[],"executionData":{}}"#,
        r#"{"inputs":[],"format_type":"dataframe"}"#,
    ] {
        reject::<ScoreDataRequest>(source);
    }
}

#[test]
fn non_nullable_optional_request_properties_reject_explicit_null() {
    for field in ["description", "sensitivityLabelSettings"] {
        let mut value = json!({"displayName": "model"});
        value[field] = Value::Null;
        assert!(serde_json::from_value::<CreateMlModelRequest>(value).is_err());
    }
    for field in ["defaultVersionAssignmentBehavior", "defaultVersionName"] {
        let mut value = json!({});
        value[field] = Value::Null;
        assert!(serde_json::from_value::<UpdateEndpointRequest>(value).is_err());
    }
    for field in ["formatType", "orientation"] {
        let mut value = json!({"inputs": []});
        value[field] = Value::Null;
        assert!(serde_json::from_value::<ScoreDataRequest>(value).is_err());
    }
}

#[test]
fn endpoint_patch_preserves_options_without_undocumented_cross_field_rules() {
    for expected in [
        json!({"defaultVersionAssignmentBehavior": "StaticallyConfigured"}),
        json!({"defaultVersionAssignmentBehavior": "NotConfigured"}),
        json!({"defaultVersionName": "v1"}),
        json!({"defaultVersionName": ""}),
        json!({
            "defaultVersionAssignmentBehavior": "NotConfigured",
            "defaultVersionName": "v1"
        }),
    ] {
        let request: UpdateEndpointRequest = serde_json::from_value(expected.clone()).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), expected);
    }
    let empty: UpdateEndpointRequest = serde_json::from_value(json!({})).unwrap();
    assert_local_error(&empty.validate().unwrap_err());
    reject::<UpdateEndpointRequest>(r#"{"defaultVersionAssignmentBehavior":"Latest"}"#);
    assert_eq!(
        serde_json::to_value(DefaultVersionAssignmentBehavior::StaticallyConfigured).unwrap(),
        json!("StaticallyConfigured")
    );
    assert_eq!(
        serde_json::to_value(DefaultVersionAssignmentBehavior::NotConfigured).unwrap(),
        json!("NotConfigured")
    );
}

#[test]
fn version_patch_requires_exact_native_scale_rule() {
    for (scale_rule, spelling) in [
        (ScaleRule::AlwaysOn, "AlwaysOn"),
        (ScaleRule::AllowScaleToZero, "AllowScaleToZero"),
    ] {
        let request = UpdateEndpointVersionRequest { scale_rule };
        request.validate().unwrap();
        let expected = json!({"scaleRule": spelling});
        assert_eq!(serde_json::to_value(&request).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<UpdateEndpointVersionRequest>(expected).unwrap(),
            request
        );
    }
    for source in [
        r#"{}"#,
        r#"{"scaleRule":null}"#,
        r#"{"scaleRule":"alwaysOn"}"#,
        r#"{"scaleRule":"Unknown"}"#,
    ] {
        reject::<UpdateEndpointVersionRequest>(source);
    }
}

#[test]
fn score_parameters_preserve_full_json_and_omit_unspecified_metadata() {
    let inputs = native_values();
    let expected = json!({"inputs": inputs});
    let request: ScoreDataRequest = serde_json::from_value(expected.clone()).unwrap();
    request.validate().unwrap();
    assert_eq!(request.inputs, inputs);
    assert_eq!(request.format_type, None);
    assert_eq!(request.orientation, None);
    assert_eq!(serde_json::to_value(request).unwrap(), expected);
    let empty: ScoreDataRequest = serde_json::from_value(json!({"inputs": []})).unwrap();
    empty.validate().unwrap();
}

#[test]
fn score_parameters_have_exact_native_format_and_orientation_enums() {
    for (orientation, spelling) in [
        (Orientation::Split, "split"),
        (Orientation::Values, "values"),
        (Orientation::Record, "record"),
        (Orientation::Index, "index"),
        (Orientation::Table, "table"),
    ] {
        let request = ScoreDataRequest {
            inputs: vec![json!([1, 2])],
            format_type: Some(FormatType::Dataframe),
            orientation: Some(orientation),
        };
        request.validate().unwrap();
        let expected = json!({
            "inputs": [[1, 2]], "formatType": "dataframe", "orientation": spelling
        });
        assert_eq!(serde_json::to_value(&request).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<ScoreDataRequest>(expected).unwrap(),
            request
        );
    }
    for source in [
        r#"{"inputs":[],"formatType":"Dataframe"}"#,
        r#"{"inputs":[],"formatType":"json"}"#,
        r#"{"inputs":[],"orientation":"records"}"#,
        r#"{"inputs":[],"orientation":"Values"}"#,
    ] {
        reject::<ScoreDataRequest>(source);
    }
    for expected in [
        json!({"inputs": [], "formatType": "dataframe"}),
        json!({"inputs": [], "orientation": "values"}),
    ] {
        let request: ScoreDataRequest = serde_json::from_value(expected.clone()).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), expected);
    }
}

#[test]
fn score_parameters_require_an_array_not_a_truthy_or_wrapped_fallback() {
    for source in [
        r#"{}"#,
        r#"{"inputs":null}"#,
        r#"{"inputs":false}"#,
        r#"{"inputs":0}"#,
        r#"{"inputs":"[]"}"#,
        r#"{"inputs":{}}"#,
        r#"{"executionData":{"inputs":[]}}"#,
        r#"{"parameters":{"inputs":[]}}"#,
    ] {
        reject::<ScoreDataRequest>(source);
    }
}

#[test]
fn direct_deserialization_rejects_duplicate_request_fields() {
    for source in [
        r#"{"displayName":"one","displayName":"two"}"#,
        r#"{"displayName":"model","description":"","description":""}"#,
        r#"{"displayName":"model","folderId":"one","folderId":"two"}"#,
        r#"{"displayName":"model","folderId":null,"folderId":null}"#,
        r#"{"displayName":"model","folderId":null,"folderId":"01234567-89ab-cdef-0123-456789abcdef"}"#,
        r#"{"displayName":"model","folderId":"01234567-89ab-cdef-0123-456789abcdef","folderId":null}"#,
        r#"{"displayName":"model","sensitivityLabelSettings":{"labelId":"one"},"sensitivityLabelSettings":{"labelId":"two"}}"#,
    ] {
        reject::<CreateMlModelRequest>(source);
    }
    reject::<UpdateMlModelRequest>(r#"{"description":"one","description":"two"}"#);
    for source in [
        r#"{"defaultVersionName":"v1","defaultVersionName":"v2"}"#,
        r#"{"defaultVersionAssignmentBehavior":"NotConfigured","defaultVersionAssignmentBehavior":"StaticallyConfigured"}"#,
    ] {
        reject::<UpdateEndpointRequest>(source);
    }
    reject::<UpdateEndpointVersionRequest>(
        r#"{"scaleRule":"AlwaysOn","scaleRule":"AllowScaleToZero"}"#,
    );
    for source in [
        r#"{"inputs":[],"inputs":[false]}"#,
        r#"{"inputs":[],"formatType":"dataframe","formatType":"dataframe"}"#,
        r#"{"inputs":[],"orientation":"values","orientation":"record"}"#,
    ] {
        reject::<ScoreDataRequest>(source);
    }
    for source in [
        r#"{"labelId":"one","labelId":"two"}"#,
        r#"{"labelId":"one","sensitivityLabelApplyStrategy":"Ignore","sensitivityLabelApplyStrategy":"ApplyOrFail"}"#,
    ] {
        reject::<SensitivityLabelSettings>(source);
    }
}

#[test]
fn native_predictions_and_future_metadata_round_trip_without_conversion() {
    let predictions = native_values();
    let expected = json!({
        "predictions": predictions,
        "formatType": "dataframe",
        "orientation": "values",
        "future": {"enabled": false, "count": 0, "missing": null, "rows": []},
        "futureArray": [],
        "futureString": "unchanged",
        "futureNull": null
    });
    let response: ScoreDataResponse = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(response.predictions, predictions);
    assert_eq!(response.format_type, Some(json!("dataframe")));
    assert_eq!(response.orientation, Some(json!("values")));
    assert_eq!(response.additional_fields["futureNull"], Value::Null);
    assert_eq!(serde_json::to_value(response).unwrap(), expected);
}

#[test]
fn response_metadata_preserves_absence_null_and_future_native_values() {
    let absent: ScoreDataResponse = serde_json::from_value(json!({"predictions": []})).unwrap();
    assert_eq!(absent.format_type, None);
    assert_eq!(absent.orientation, None);
    assert!(absent.additional_fields.is_empty());
    assert_eq!(
        serde_json::to_value(absent).unwrap(),
        json!({"predictions": []})
    );
    for (format_type, orientation) in [
        (Value::Null, Value::Null),
        (json!("future-format"), json!("future-orientation")),
        (json!(false), json!(0)),
        (json!([]), json!({"future": null})),
    ] {
        let expected = json!({
            "predictions": [],
            "formatType": format_type,
            "orientation": orientation
        });
        let response: ScoreDataResponse = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(response.format_type, Some(format_type));
        assert_eq!(response.orientation, Some(orientation));
        assert_eq!(serde_json::to_value(response).unwrap(), expected);
    }
}

#[test]
fn score_response_requires_predictions_array_even_when_empty() {
    for source in [
        r#"{}"#,
        r#"{"predictions":null}"#,
        r#"{"predictions":false}"#,
        r#"{"predictions":0}"#,
        r#"{"predictions":"[]"}"#,
        r#"{"predictions":{}}"#,
        r#"{"status":"Succeeded"}"#,
        r#"{"results":{"predictions":[]}}"#,
    ] {
        reject::<ScoreDataResponse>(source);
    }
    let response: ScoreDataResponse = serde_json::from_value(json!({"predictions": []})).unwrap();
    assert!(response.predictions.is_empty());
}

#[test]
fn accepted_scoring_retains_the_exact_operation_id_without_predictions() {
    for operation_id in [None, Some(GUID.to_owned())] {
        let outcome = ScoreOutcome::from_resource(ResourceOutcome::Accepted {
            operation_id: operation_id.clone(),
        })
        .unwrap();
        assert_eq!(outcome, ScoreOutcome::Accepted { operation_id });
    }
}

#[test]
fn only_http_200_with_native_predictions_is_completed_scoring() {
    for predictions in [vec![], native_values()] {
        let data = json!({"predictions": predictions, "future": false});
        let outcome = ScoreOutcome::from_resource(ResourceOutcome::Completed {
            status: 200,
            data: Some(data.clone()),
        })
        .unwrap();
        let ScoreOutcome::Completed(response) = outcome else {
            panic!("native HTTP 200 predictions must be completed");
        };
        assert_eq!(serde_json::to_value(response).unwrap(), data);
    }
}

#[test]
fn malformed_completed_score_is_a_sanitized_error_with_effect_evidence() {
    for data in [
        None,
        Some(Value::Null),
        Some(json!([])),
        Some(json!(false)),
        Some(json!({"status": "Succeeded", "private": "do-not-log"})),
        Some(json!({"predictions": null, "private": "do-not-log"})),
        Some(json!({"predictions": false})),
        Some(json!({"predictions": 0})),
        Some(json!({"predictions": "do-not-log"})),
        Some(json!({"predictions": {}})),
    ] {
        let error = ScoreOutcome::from_resource(ResourceOutcome::Completed { status: 200, data })
            .unwrap_err();
        assert_eq!(error.code, "InvalidResponse");
        assert_eq!(error.status, Some(200));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(error.operation_id, None);
        assert!(error.service_error.is_none());
        assert!(error.raw_service_error.is_none());
        assert!(!format!("{error} {error:?}").contains("do-not-log"));
    }
}

#[test]
fn unexpected_completed_status_is_not_accepted_or_success() {
    for status in [201, 202, 204, 301, 400, 429, 500] {
        for data in [
            None,
            Some(json!({"predictions": [false, 0, null], "private": "do-not-log"})),
            Some(json!({"error": {"message": "do-not-log"}})),
        ] {
            let error = ScoreOutcome::from_resource(ResourceOutcome::Completed { status, data })
                .unwrap_err();
            assert_eq!(error.code, "OperationFailed");
            assert_eq!(error.status, Some(status));
            assert!(error.mutation_may_have_occurred);
            assert_eq!(error.operation_id, None);
            assert!(error.service_error.is_none());
            assert!(error.raw_service_error.is_none());
            assert!(!format!("{error} {error:?}").contains("do-not-log"));
        }
    }
}

#[test]
fn debug_never_formats_arguments_predictions_metadata_or_operation_identifiers() {
    let secret = "private-do-not-log";
    let label = SensitivityLabelSettings {
        label_id: secret.into(),
        sensitivity_label_apply_strategy: None,
    };
    let create = CreateMlModelRequest {
        display_name: secret.into(),
        description: Some(secret.into()),
        folder_id: Some(Some(secret.into())),
        sensitivity_label_settings: Some(label.clone()),
    };
    let update = UpdateMlModelRequest {
        description: secret.into(),
    };
    let endpoint = UpdateEndpointRequest {
        default_version_assignment_behavior: None,
        default_version_name: Some(secret.into()),
    };
    let request = ScoreDataRequest {
        inputs: vec![json!(secret)],
        format_type: None,
        orientation: None,
    };
    let response: ScoreDataResponse = serde_json::from_value(json!({
        "predictions": [secret],
        "formatType": secret,
        "orientation": secret,
        "future": secret
    }))
    .unwrap();
    let completed = ScoreOutcome::Completed(response.clone());
    let accepted = ScoreOutcome::Accepted {
        operation_id: Some(secret.into()),
    };
    for debug in [
        format!("{label:?}"),
        format!("{create:?}"),
        format!("{update:?}"),
        format!("{endpoint:?}"),
        format!("{request:?}"),
        format!("{response:?}"),
        format!("{completed:?}"),
        format!("{accepted:?}"),
        format!("{:?}", create.validate().unwrap_err()),
    ] {
        assert!(!debug.contains(secret));
    }
}
