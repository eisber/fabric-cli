// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Process lifecycle shared by the native executable and offline process tests.

use crate::Host;
use fabric_core::CancellationToken;
use std::{ffi::OsString, io::Write};

/// Run the production parser, runtime, signals, streams, and command exit mapping.
///
/// The executable supplies its native host. Tests can supply offline effects
/// without adding an environment-controlled authentication bypass to `fab-rs`.
pub fn run_process(args: Vec<OsString>, host: &dyn Host) -> u8 {
    if crate::welcome_requested(&args) {
        return crate::welcome(&mut std::io::stdout(), &mut std::io::stderr());
    }
    if let Err(error) = crate::parse_arguments(&args)
        && matches!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
        )
    {
        return if std::io::stdout()
            .write_all(error.to_string().as_bytes())
            .is_ok()
        {
            0
        } else {
            1
        };
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("Cannot initialize fab-rs runtime");
            return 1;
        }
    };
    runtime.block_on(async {
        let cancellation = CancellationToken::new();
        let signal = cancellation.clone();
        #[cfg(unix)]
        let listener = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
        #[cfg(windows)]
        let listener = tokio::signal::windows::ctrl_c();
        let mut listener = match listener {
            Ok(listener) => listener,
            Err(_) => {
                eprintln!("Cannot initialize fab-rs Ctrl+C handler");
                return 1;
            }
        };
        let handler = tokio::spawn(async move {
            if listener.recv().await.is_none() {
                eprintln!("fab-rs Ctrl+C handler closed unexpectedly");
            }
            signal.cancel();
        });
        let code = crate::run(
            args,
            host,
            cancellation,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
        .await;
        handler.abort();
        code
    })
}
