// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Unexpected success-class operation results retain Report uncertainty.

use super::*;
use fabric_sdk_operations::OperationErrorKind;

#[tokio::test]
async fn unexpected_success_result_status_is_unknown_with_original_typed_error_and_submission() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for status in [201, 202, 204] {
        let prepared = prepare(temp.path(), &["--wait", "--project", "missing"], &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(json!([]));
        let mut acceptance = accepted();
        let mut submitted = metadata();
        submitted["folderId"] = Value::Null;
        acceptance.body = serde_json::to_vec(&submitted).unwrap();
        let wire = Wire::new(vec![
            acceptance,
            response(200, json!({"status":"Succeeded"})),
            OperationResponse {
                status,
                body: if status == 204 {
                    Vec::new()
                } else {
                    serde_json::to_vec(&json!({"unexpectedResult":true})).unwrap()
                },
                headers: BTreeMap::new(),
            },
        ]);
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                execution(&wire),
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_ne!(delivery.report().exit_code(), 0);
        let error = delivery.report().error().unwrap();
        assert!(
            matches!(&error.kind, OperationErrorKind::Http { status: actual, .. } if *actual == status)
        );
        assert_eq!(error.operation_id.as_deref(), Some(OP));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(delivery.report().value()["meta"]["status"], "unknown");
        assert_eq!(
            delivery.report().value()["error"]["code"],
            "InvalidResponse"
        );
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(receipt["data"], Value::Null);
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["meta"]["httpStatus"], status);
        assert_eq!(receipt["meta"]["resourceId"], REPORT);
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], REPORT);
        assert_eq!(
            receipt["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["submission"]["folderId"], Value::Null);
        assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], true);
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        assert_eq!(receipt["meta"]["remoteCancelled"], false);
        assert_eq!(approval.calls, 1);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.0 == ResourceMethod::Post)
                .count(),
            1
        );
        assert_eq!(requests[0].2, raw);
        assert_eq!(
            requests[2].1,
            format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result")
        );
    }
}

#[tokio::test]
async fn genuine_provider_failure_keeps_failed_classification_and_original_cause() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for operation_failure in [false, true] {
        let prepared = prepare(temp.path(), &["--wait"], &raw);
        let (client, _) = client(Audience::Fabric);
        let listing = Listing::new(json!([]));
        let mut acceptance = accepted();
        acceptance.body = serde_json::to_vec(&metadata()).unwrap();
        let wire = Wire::new(vec![
            acceptance,
            if operation_failure {
                response(
                    200,
                    json!({"status":"Failed","error":{"errorCode":"NativeFailure"}}),
                )
            } else {
                response(403, json!({"errorCode":"InsufficientPrivileges"}))
            },
        ]);
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &listing,
                execution(&wire),
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_ne!(delivery.report().exit_code(), 0);
        let error = delivery.report().error().unwrap();
        if operation_failure {
            assert!(matches!(
                &error.kind,
                OperationErrorKind::OperationFailed { .. }
            ));
            assert_eq!(
                delivery.report().value()["error"]["code"],
                "OperationFailed"
            );
        } else {
            assert!(matches!(
                &error.kind,
                OperationErrorKind::Http { status: 403, .. }
            ));
            assert_eq!(
                delivery.report().value()["error"]["code"],
                "ServiceRejected"
            );
        }
        assert_eq!(delivery.report().value()["meta"]["status"], "failed");
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        assert_eq!(
            delivery.report().value()["meta"]["submission"]["resource"]["id"],
            REPORT
        );
        assert_eq!(
            delivery.report().value()["meta"]["submission"]["httpStatus"],
            202
        );
        assert_eq!(error.operation_id.as_deref(), Some(OP));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(wire.requests.lock().unwrap().len(), 2);
    }
}
