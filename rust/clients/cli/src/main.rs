// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `fab-rs` runtime, cancellation, and standard-stream ownership.
#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(fabric_cli::lifecycle::run_process(
        std::env::args_os().collect(),
        &fabric_cli::NativeHost::default(),
    ))
}
