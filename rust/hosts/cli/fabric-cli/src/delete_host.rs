// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::batch_state::CommandOutcome;
use fabric_cli::{
    delete::{DeleteEnvelope, NativeDeleteFiles, PreparedDelete, preflight, prepare_output},
    parser::DeleteCommand,
};
use std::io::{self, Write};
use std::time::Instant;

pub(crate) fn run(command: &DeleteCommand) -> io::Result<CommandOutcome> {
    let prepared = match preflight(command, None) {
        Ok(prepared) => prepared,
        Err(error) => {
            return write_failure(&error.envelope(Some(command)), error.exit_code());
        }
    };
    let deadline = Instant::now()
        .checked_add(prepared.timeout())
        .ok_or_else(|| io::Error::other("The deletion deadline is outside the clock range"))?;
    let base = match std::env::current_dir() {
        Ok(base) => base,
        Err(_) => {
            return host_failure(
                &prepared,
                "OutputFailed",
                "The local output base could not be captured.",
            );
        }
    };
    let mut files = match NativeDeleteFiles::new(base) {
        Ok(files) => files,
        Err(error) => {
            return write_failure(&error.envelope(Some(command)), error.exit_code());
        }
    };
    let output = match prepare_output(&prepared, &mut files) {
        Ok(output) => output,
        Err(error) => {
            return host_failure(&prepared, error.code(), error.message());
        }
    };
    if Instant::now() >= deadline {
        return host_failure(
            &prepared,
            "Timeout",
            "The total deletion deadline elapsed before submission.",
        );
    }
    #[cfg(feature = "native-host")]
    {
        crate::native_read::run_delete(prepared, output, deadline)
    }
    #[cfg(not(feature = "native-host"))]
    {
        let _ = output;
        host_failure(
            &prepared,
            "NotConfigured",
            "Delete execution requires an explicitly configured native host.",
        )
    }
}

pub(crate) fn host_failure(
    prepared: &PreparedDelete,
    code: &str,
    message: &str,
) -> io::Result<CommandOutcome> {
    let envelope =
        fabric_cli::delete::host_failure(prepared, code, message).map_err(io::Error::other)?;
    write_failure(&envelope, 1)
}

pub(crate) fn write_failure(envelope: &DeleteEnvelope, exit: u8) -> io::Result<CommandOutcome> {
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(&mut stderr, envelope).map_err(io::Error::other)?;
    writeln!(stderr)?;
    stderr.flush()?;
    Ok(failure_outcome(
        envelope.error.as_ref().map(|error| error.code.as_str()),
        exit,
    ))
}

fn failure_outcome(code: Option<&str>, exit: u8) -> CommandOutcome {
    match code {
        Some("Cancelled") => CommandOutcome::Cancelled(exit),
        Some("Timeout" | "SignalFailed" | "OutputFailed" | "ProjectionFailed") => {
            CommandOutcome::Stop(exit)
        }
        _ => CommandOutcome::Returned(exit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interruption_and_output_failure_stop_enclosing_batches() {
        assert_eq!(
            failure_outcome(Some("Cancelled"), 1),
            CommandOutcome::Cancelled(1)
        );
        for code in [
            "Timeout",
            "SignalFailed",
            "OutputFailed",
            "ProjectionFailed",
        ] {
            assert_eq!(failure_outcome(Some(code), 1), CommandOutcome::Stop(1));
        }
        assert_eq!(
            failure_outcome(Some("AccessDenied"), 1),
            CommandOutcome::Returned(1)
        );
    }
}
