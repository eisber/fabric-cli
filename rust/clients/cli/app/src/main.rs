// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `fab-rs` runtime, cancellation, and standard-stream ownership.
#![forbid(unsafe_code)]

use fabric_core::CancellationToken;
use std::process::ExitCode;

fn main() -> ExitCode {
    // Help/version do not even require runtime creation or signal initialization.
    let args: Vec<_> = std::env::args_os().collect();
    if let Err(error) = fabric_cli::parser().try_get_matches_from(&args)
        && matches!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
        )
    {
        use std::io::Write;
        return ExitCode::from(
            if std::io::stdout()
                .write_all(error.to_string().as_bytes())
                .is_ok()
            {
                0
            } else {
                1
            },
        );
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("Cannot initialize fab-rs runtime");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(async {
        let cancellation = CancellationToken::new();
        let signal = cancellation.clone();
        let handler = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal.cancel();
            }
        });
        let code = fabric_cli::run(
            args,
            &fabric_cli::NativeHost::default(),
            cancellation,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
        .await;
        handler.abort();
        code
    });
    ExitCode::from(code)
}
