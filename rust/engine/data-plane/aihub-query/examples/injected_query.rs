// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Complete offline consumer: injected HTTP and credentials, real SDK lifecycle.

#![deny(unnameable_test_items)]

mod support;

use fabric_sdk_aihub_query::{Limits, NativeValue, QueryOutcome};
use std::{future::pending, sync::Arc};
use support::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wire = Wire::new(setup().into_iter().chain([rpc(3, success())]));
    let credential = Arc::new(Credential::default());
    let client = client(
        wire.clone(),
        credential,
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let outcome = client
        .execute_query(&request(), &deadline(), pending())
        .await?;
    let QueryOutcome::Complete(result) = outcome else {
        return Err("result was truncated; no complete export is permitted".into());
    };
    assert_eq!(result.resultsets.len(), 1);
    assert_eq!(
        result.resultsets[0].rows[0][0],
        NativeValue::Integer("18446744073709551615".into())
    );
    assert_eq!(
        result.resultsets[0].rows[0][1],
        NativeValue::Decimal("0.1000000000000000000000000001".into())
    );
    let requests = messages(&wire);
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call"
        ]
    );
    for request in requests {
        assert_eq!(
            request["params"]["_meta"]["variants"],
            "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.DataAgent.TypedQueryResults"
        );
    }
    println!(
        "Validated {} typed resultset without local export or live service I/O.",
        result.resultsets.len()
    );
    Ok(())
}
