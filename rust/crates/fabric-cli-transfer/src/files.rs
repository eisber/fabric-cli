// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{TransferEvent, TransferFiles, TransferUi};
use fabric_deploy::definitions::{
    DefinitionError, DefinitionErrorKind, DefinitionFile, DefinitionFileSystem,
    DefinitionImportFileSystem,
};
use fabric_sdk_operations::{OperationExecutor, OperationSession};
use std::{
    future::{Future, poll_fn},
    io,
    path::{Path, PathBuf},
    pin::pin,
    task::Poll,
};

pub(crate) async fn export_directory<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    files: &mut dyn TransferFiles,
    output: &str,
) -> Result<PathBuf, DefinitionError> {
    file_phase(session, files.resolve_export_directory_async(output))
        .await
        .map_err(|(error, _)| error)
}

pub(crate) struct ImportFiles<'a, T: ?Sized>(pub(crate) &'a mut T);

impl<T: TransferFiles + ?Sized> DefinitionImportFileSystem for ImportFiles<'_, T> {
    fn entries(
        &mut self,
        root: &Path,
        relative: &str,
    ) -> io::Result<Vec<fabric_deploy::definitions::DefinitionEntry>> {
        self.0.entries(root, relative)
    }

    fn read(&mut self, root: &Path, relative: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        self.0.read(root, relative, max_bytes)
    }
}

/// Capture the released codec's bounded write plan, without filesystem effects.
///
/// The synchronous definition library still owns decoding, path validation,
/// service order and empty-item directory selection. Actual preflight and writes
/// are deferred to the caller's asynchronous file capability, in the same order.
#[derive(Default)]
pub(crate) struct ExportFiles {
    files: Vec<DefinitionFile>,
    directory: Option<String>,
}

impl DefinitionFileSystem for ExportFiles {
    fn preflight(&mut self, _: &Path, files: &[DefinitionFile]) -> io::Result<()> {
        self.files = files
            .iter()
            .map(|file| DefinitionFile {
                path: file.path.clone(),
                bytes: file.bytes.clone(),
            })
            .collect();
        Ok(())
    }

    fn create_directory(&mut self, _: &Path, name: &str) -> io::Result<()> {
        self.directory = Some(name.into());
        Ok(())
    }

    fn write(&mut self, _: &Path, _: &DefinitionFile) -> io::Result<()> {
        Ok(())
    }
}

impl ExportFiles {
    pub(crate) fn preparation_error(mut error: DefinitionError) -> DefinitionError {
        // The released library visited the in-memory plan, not the filesystem.
        error.completed_paths.clear();
        error.incomplete_path = None;
        error
    }

    pub(crate) async fn write<E: OperationExecutor>(
        self,
        session: &mut OperationSession<'_, E>,
        filesystem: &mut dyn TransferFiles,
        root: &Path,
        ui: &dyn TransferUi,
        progress: String,
    ) -> Result<Vec<String>, DefinitionError> {
        check_cancelled(session).await?;
        let files = file_phase(session, filesystem.preflight_async(root, self.files))
            .await
            .map_err(|(error, _)| error)?;
        if let Some(directory) = self.directory {
            check_cancelled(session).await?;
            file_phase(session, filesystem.create_directory_async(root, &directory))
                .await
                .map_err(|(error, _)| error)?;
        }
        ui.emit(TransferEvent::Grey(progress)).map_err(|_| {
            DefinitionErrorKind::FileSystem(io::Error::other("Transfer progress output failed"))
        })?;
        let mut completed_paths = Vec::new();
        for file in files {
            if let Err(mut error) = check_cancelled(session).await {
                error.completed_paths = completed_paths;
                return Err(error);
            }
            let path = file.path.clone();
            if let Err((mut error, started)) =
                file_phase(session, filesystem.write_async(root, file)).await
            {
                error.completed_paths = completed_paths;
                error.incomplete_path = started.then_some(path);
                return Err(error);
            }
            completed_paths.push(path);
        }
        if let Err(mut error) = check_cancelled(session).await {
            error.completed_paths = completed_paths;
            return Err(error);
        }
        Ok(completed_paths)
    }
}

async fn file_phase<T, E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    work: impl Future<Output = io::Result<T>>,
) -> Result<T, (DefinitionError, bool)> {
    let mut work = pin!(work);
    let mut started = false;
    poll_fn(|cx| {
        let mut checkpoint = pin!(session.check_cancelled());
        if let Poll::Ready(Err(error)) = checkpoint.as_mut().poll(cx) {
            return Poll::Ready(Err((
                DefinitionErrorKind::Operation(Box::new(error)).into(),
                started,
            )));
        }
        started = true;
        work.as_mut().poll(cx).map(|result| {
            result.map_err(|error| (DefinitionErrorKind::FileSystem(error).into(), true))
        })
    })
    .await
}

async fn check_cancelled<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
) -> Result<(), DefinitionError> {
    session
        .check_cancelled()
        .await
        .map_err(|error| DefinitionErrorKind::Operation(Box::new(error)).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_failure_never_claims_captured_plan_paths_were_written() {
        let error = ExportFiles::preparation_error(DefinitionError {
            kind: DefinitionErrorKind::FileSystem(io::Error::from(io::ErrorKind::Interrupted)),
            completed_paths: vec!["planned-only.bin".into()],
            incomplete_path: Some("also-planned.bin".into()),
        });
        assert!(error.completed_paths.is_empty());
        assert!(error.incomplete_path.is_none());
        assert!(matches!(error.kind, DefinitionErrorKind::FileSystem(error)
                if error.kind() == io::ErrorKind::Interrupted));
    }
}
