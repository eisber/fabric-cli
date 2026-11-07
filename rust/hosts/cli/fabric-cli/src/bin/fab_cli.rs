// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional discovery-only head. No v1 dispatcher, login, config or data host.

use fabric_cli::v2_discovery::DiscoveryHost;
use futures_util::FutureExt;
use std::{
    future::pending,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let host = DiscoveryHost::default();
    let (mut stdout, mut stderr) = (io::stdout().lock(), io::stderr().lock());
    // With no data provider every branch is pure and immediately ready. Do not
    // create a runtime, transport or identity host merely to print a descriptor.
    let result = host
        .run(std::env::args_os(), pending(), &mut stdout, &mut stderr)
        .now_or_never();
    match result {
        Some(Ok(0)) => ExitCode::SUCCESS,
        Some(Ok(2)) => ExitCode::from(2),
        Some(Ok(_)) => ExitCode::FAILURE,
        Some(Err(_)) | None => {
            // Best effort only at the process boundary; the public host returns
            // the actual writer error and never reports partial output as success.
            let _ = stderr.write_all(
                b"{\"error\":{\"code\":\"output_failed\",\"message\":\"Discovery output could not be completed.\"}}\n",
            );
            let _ = stderr.flush();
            ExitCode::FAILURE
        }
    }
}
