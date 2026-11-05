// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The facade shares native scoring parameters and results without CLI types.

#![deny(unnameable_test_items)]

use fabric_sdk::items::ml_models::{
    MlModelEffect, MlModelIdentity, MlModelRequests, MlModelTarget, ScoreDataRequest, ScoreOutcome,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::json;
use std::{future::Future, pin::Pin};

struct Capture {
    requests: Vec<ResourceOperation>,
    outcome: ResourceOutcome,
}

impl ResourceOperations for Capture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.requests.push(operation);
        Box::pin(std::future::ready(Ok(self.outcome.clone())))
    }
}

fn target() -> MlModelTarget {
    MlModelTarget::new(
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
        "MLModel",
    )
    .unwrap()
}

#[tokio::test]
async fn facade_delegates_native_parameters_and_predictions_without_truthiness_loss() {
    let parameters = json!({
        "inputs": [[false, 0, null, "", [], {"nested": [1, true]}]],
        "formatType": "dataframe",
        "orientation": "values"
    });
    let input: ScoreDataRequest = serde_json::from_value(parameters.clone()).unwrap();
    let native = json!({
        "predictions": [[false], [0], [null], [""], [[]]],
        "formatType": "dataframe",
        "orientation": "values",
        "futureNativeField": {"present": false}
    });
    let mut executor = Capture {
        requests: Vec::new(),
        outcome: ResourceOutcome::Completed {
            status: 200,
            data: Some(native.clone()),
        },
    };
    let request = MlModelRequests::new(MlModelIdentity::ServicePrincipal)
        .score_version(&target(), "01", &input, true)
        .unwrap();
    assert_eq!(request.effect(), MlModelEffect::ScoreVersion);
    assert_eq!(
        request.operation().completion(),
        CompletionPolicy::WaitForResult
    );
    let outcome = request.execute(&mut executor).await.unwrap();
    let ScoreOutcome::Completed(output) = ScoreOutcome::from_resource(outcome).unwrap() else {
        panic!("completed native predictions required");
    };
    assert_eq!(serde_json::to_value(output).unwrap(), native);
    assert_eq!(executor.requests.len(), 1);
    assert_eq!(executor.requests[0].body(), Some(&parameters));
    assert!(
        executor.requests[0]
            .path()
            .ends_with("/endpoint/versions/01/score")
    );
}

#[tokio::test]
async fn facade_keeps_acceptance_separate_from_predictions_without_extra_requests() {
    let operation_id = Some("33333333-3333-4333-8333-333333333333".to_owned());
    let mut executor = Capture {
        requests: Vec::new(),
        outcome: ResourceOutcome::Accepted {
            operation_id: operation_id.clone(),
        },
    };
    let input: ScoreDataRequest = serde_json::from_value(json!({"inputs": []})).unwrap();
    let request = MlModelRequests::new(MlModelIdentity::ManagedIdentity)
        .score_default(&target(), &input, false)
        .unwrap();
    assert_eq!(
        request.operation().completion(),
        CompletionPolicy::ReturnAccepted
    );
    assert_eq!(
        ScoreOutcome::from_resource(request.execute(&mut executor).await.unwrap()).unwrap(),
        ScoreOutcome::Accepted { operation_id }
    );
    assert_eq!(executor.requests.len(), 1);
    assert!(
        executor.requests[0]
            .path()
            .contains("/mlModels/22222222-2222-4222-8222-222222222222/endpoint/score")
    );
}
