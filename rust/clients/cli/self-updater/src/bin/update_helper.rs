// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Installed, build-pinned bounded helper. Never a downloaded update payload.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let result = fabric_cli_update::HelperPolicy::production().and_then(|policy| {
        fabric_cli_update::run_helper(policy, &std::env::args_os().skip(1).collect::<Vec<_>>())
    });
    match result {
        Ok(result) => {
            match serde_json::to_string(&result) {
                Ok(json) => println!("{json}"),
                Err(_) => return ExitCode::FAILURE,
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
