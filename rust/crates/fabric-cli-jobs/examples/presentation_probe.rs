// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual offline job output consumer for the unchanged Python stream comparison.

#[path = "../../../tests/compatibility/jobs/presentation_consumer.rs"]
mod consumer;

use std::io::{self, BufRead};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!(
            "{}",
            consumer::observe(&serde_json::from_str(&line?)?).await?
        );
    }
    Ok(())
}
