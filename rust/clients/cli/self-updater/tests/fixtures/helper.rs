// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic pinned helper used only by the offline lifecycle fixture.

#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use std::{fs, process::ExitCode};

use fabric_cli_update::{Channel, Error, HelperPolicy, HelperState, run_helper_with_fault};

mod common;

fn main() -> ExitCode {
    if let Ok(path) = std::env::current_exe() {
        let _ = fs::write(
            path.with_file_name(".fixture-helper-runtime.json"),
            serde_json::json!({
                "system_root":std::env::var_os("SystemRoot").map(std::path::PathBuf::from)
            })
            .to_string(),
        );
    }
    let fault = std::env::current_exe()
        .ok()
        .and_then(|path| fs::read_to_string(path.with_file_name(".fixture-fault")).ok())
        .unwrap_or_default();
    let mut hook = |phase| {
        if phase == HelperState::OldDisplaced && fault == "old-rename-death" {
            std::process::exit(86);
        }
        if (phase == HelperState::OldDisplaced && fault == "old-rename-fail")
            || (phase == HelperState::Applied && fault == "receipt-fail")
        {
            Err(Error::Io)
        } else {
            Ok(())
        }
    };
    let policy = common::trust().and_then(|trust| {
        HelperPolicy::new(trust, Channel::Stable, common::target()?, common::VARIANT)
    });
    match policy.and_then(|policy| {
        run_helper_with_fault(
            policy,
            &std::env::args_os().skip(1).collect::<Vec<_>>(),
            &mut hook,
        )
    }) {
        Ok(receipt) => {
            println!(
                "{}",
                serde_json::to_string(&receipt).expect("fixture result")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            if let Ok(path) = std::env::current_exe() {
                let _ = fs::write(
                    path.with_file_name(".fixture-helper-error.json"),
                    serde_json::json!({"error":error.to_string()}).to_string(),
                );
            }
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
