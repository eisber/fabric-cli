// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone public TABLE SDK -> generic jobs -> shared HTTP offline consumer.

#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_sdk_jobs::WaitOptions;
use fabric_sdk_tables::{
    JobMaintenance, Maintenance, MaintenanceExecutor, MaintenanceOutcome, TablePath,
};
use std::{future::pending, sync::Arc};
use support::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let http = ScriptedHttp::new([
        accepted(),
        response(
            200,
            r#"{"id":"run","status":"InProgress"}"#,
            &[("Retry-After", "1")],
        ),
        response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
    ]);
    let jobs = sdk(http.clone(), Arc::new(TestTimer::default()));
    let adapter = JobMaintenance {
        jobs: &jobs,
        wait: WaitOptions::default(),
        cancel_on_timeout: true,
        operations: None,
    };
    let table = TablePath::new("ws", "item", "Tables/dbo/sales")?;
    let config = Maintenance::Optimize {
        vorder: true,
        zorder: Some(vec!["key".into()]),
    }
    .execution_data(&table);
    let outcome = adapter
        .run_observed(
            &table,
            config,
            &mut |event| {
                println!("{event:?}");
                Ok(())
            },
            Box::pin(pending()),
        )
        .await?;
    assert_eq!(
        outcome,
        MaintenanceOutcome::Completed {
            instance_id: "run".into()
        }
    );
    assert_eq!(http.requests.lock().unwrap().len(), 3);
    Ok(())
}
