// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional discovery head. Only bare data schema can select the native host.

#![cfg_attr(test, deny(unnameable_test_items))]

#[cfg(all(
    feature = "native-host",
    any(feature = "windows-broker", feature = "interactive")
))]
#[path = "../auth.rs"]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod auth;
#[cfg(all(feature = "native-host", windows, feature = "windows-broker"))]
#[path = "../auth_binding.rs"]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod auth_binding;
#[cfg(feature = "native-host")]
#[path = "../batch_state.rs"]
#[allow(dead_code, reason = "Legacy state and dispatch are not mounted here.")]
mod batch_state;
#[cfg(feature = "native-host")]
#[path = "../delete_host.rs"]
#[allow(dead_code, reason = "Legacy deletion is not mounted here.")]
mod delete_host;
#[cfg(feature = "headless")]
#[path = "../headless.rs"]
#[allow(
    dead_code,
    reason = "Only explicit native credential selection is reused."
)]
mod headless;
#[cfg(feature = "interactive")]
#[path = "../interactive.rs"]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod interactive;
#[cfg(feature = "interactive")]
#[path = "../interactive_binding.rs"]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod interactive_binding;
#[cfg(feature = "native-host")]
#[path = "../local_host.rs"]
#[allow(
    dead_code,
    reason = "Only the UTC clock is reused; no local commands execute."
)]
mod local_host;
#[cfg(feature = "native-host")]
#[path = "../native_read.rs"]
#[allow(
    dead_code,
    reason = "Only the data-schema entry point is mounted here."
)]
mod native_read;
#[cfg(feature = "native-host")]
#[path = "../runtime_config.rs"]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod runtime_config;
#[cfg(feature = "native-host")]
#[path = "../v2_schema_native/host.rs"]
mod schema;

use fabric_cli::v2_discovery::DiscoveryHost;
use futures_util::FutureExt;
use std::{
    future::pending,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let (mut stdout, mut stderr) = (io::stdout().lock(), io::stderr().lock());
    let result = run(std::env::args_os().collect(), &mut stdout, &mut stderr);
    match result {
        Ok(0) => ExitCode::SUCCESS,
        Ok(2) => ExitCode::from(2),
        Ok(_) => ExitCode::FAILURE,
        Err(_) => {
            let _ = stderr.write_all(
                b"{\"error\":{\"code\":\"output_failed\",\"message\":\"Discovery output could not be completed.\"}}\n",
            );
            let _ = stderr.flush();
            ExitCode::FAILURE
        }
    }
}

fn run(
    args: Vec<std::ffi::OsString>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    #[cfg(feature = "native-host")]
    if let Ok(fabric_cli::v2_discovery::ParsedDiscovery::Request(
        fabric_cli::v2_discovery::DiscoveryRequest::DataSchema(request),
    )) = fabric_cli::v2_discovery::parse_from(args.clone())
    {
        return schema::run(&request, stderr);
    }
    let host = DiscoveryHost::default();
    // With no data provider every branch is pure and immediately ready. Do not
    // create a runtime, transport or identity host merely to print a descriptor.
    let result = host.run(args, pending(), stdout, stderr).now_or_never();
    match result {
        Some(result) => result,
        None => Err(io::Error::other("Local discovery did not complete.")),
    }
}
