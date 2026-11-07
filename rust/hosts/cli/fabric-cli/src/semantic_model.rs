// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Binary-owned local file and stream composition; no identity/configuration I/O.

use crate::{CommandError, CommandOutcome, write_error};
use fabric_cli_semantic_model::{AnalyzeCommand, CommandError as AnalysisCommandError};
use std::io;

pub(super) fn run(command: &AnalyzeCommand) -> io::Result<CommandOutcome> {
    if let Err(error) = command.preflight() {
        return command_failure(error);
    }
    #[cfg(not(feature = "semantic-model-dotnet"))]
    {
        command_failure(AnalysisCommandError::Unsupported(
            "BPA execution requires the semantic-model-dotnet feature. No files were opened.",
        ))
    }
    #[cfg(feature = "semantic-model-dotnet")]
    {
        native::run(command)
    }
}

fn command_failure(error: AnalysisCommandError) -> io::Result<CommandOutcome> {
    let exit = if error.code() == "InvalidInput" { 2 } else { 1 };
    write_error(
        CommandError {
            code: error.code(),
            message: error.to_string(),
        },
        exit,
    )?;
    Ok(CommandOutcome::Returned(exit))
}

#[cfg(feature = "semantic-model-dotnet")]
mod native {
    use super::*;
    use fabric_cli_semantic_model::{AnalysisFiles, execute, write_report};
    use std::{fs::File, io::Read, path::Path, sync::atomic::AtomicBool};

    struct LocalFiles;
    impl AnalysisFiles for LocalFiles {
        fn read(&mut self, path: &Path, max_bytes: usize) -> Result<String, AnalysisCommandError> {
            let file = File::open(path).map_err(|_| AnalysisCommandError::InputRead)?;
            if !file
                .metadata()
                .map_err(|_| AnalysisCommandError::InputRead)?
                .is_file()
            {
                return Err(AnalysisCommandError::InputRead);
            }
            let mut bytes = Vec::new();
            file.take(u64::try_from(max_bytes).map_err(|_| AnalysisCommandError::Limit)? + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| AnalysisCommandError::InputRead)?;
            if bytes.len() > max_bytes {
                return Err(AnalysisCommandError::Limit);
            }
            String::from_utf8(bytes).map_err(|_| AnalysisCommandError::InvalidInput)
        }
    }

    pub(super) fn run(command: &AnalyzeCommand) -> io::Result<CommandOutcome> {
        let backend = match fabric_cli_semantic_model::native_backend(command) {
            Ok(backend) => backend,
            Err(error) => return command_failure(error),
        };
        match execute(command, &mut LocalFiles, &backend, &AtomicBool::new(false)) {
            Ok(report) => {
                let exit = write_report(command, &report, &mut io::stdout().lock())?;
                Ok(CommandOutcome::Returned(exit))
            }
            Err(error) => command_failure(error),
        }
    }
}
