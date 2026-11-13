// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{io, path::Path, sync::Arc};
use tantivy::directory::{
    Directory, DirectoryLock, FileHandle, Lock, META_LOCK, WatchCallback, WatchHandle, WritePtr,
    error::{DeleteError, LockError, OpenReadError, OpenWriteError},
};

#[derive(Clone, Debug)]
pub(crate) struct EmbeddedDirectory {
    files: &'static [(&'static str, &'static [u8])],
}

impl EmbeddedDirectory {
    pub(crate) const fn new(files: &'static [(&'static str, &'static [u8])]) -> Self {
        Self { files }
    }

    fn bytes(&self, path: &Path) -> Result<&'static [u8], OpenReadError> {
        self.files
            .iter()
            .find(|(name, _)| Path::new(name) == path)
            .map(|(_, bytes)| *bytes)
            .ok_or_else(|| OpenReadError::FileDoesNotExist(path.to_path_buf()))
    }
}

fn read_only() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Embedded docs index is immutable.",
    )
}

impl Directory for EmbeddedDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        Ok(Arc::new(self.bytes(path)?))
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        Ok(self.files.iter().any(|(name, _)| Path::new(name) == path))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        Ok(self.bytes(path)?.to_vec())
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        Err(OpenWriteError::wrap_io_error(
            read_only(),
            path.to_path_buf(),
        ))
    }

    fn atomic_write(&self, _path: &Path, _data: &[u8]) -> io::Result<()> {
        Err(read_only())
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        if self.files.iter().any(|(name, _)| Path::new(name) == path) {
            Err(DeleteError::IoError {
                io_error: Arc::new(read_only()),
                filepath: path.to_path_buf(),
            })
        } else {
            Err(DeleteError::FileDoesNotExist(path.to_path_buf()))
        }
    }

    fn sync_directory(&self) -> io::Result<()> {
        Ok(())
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        // Immutable snapshots cannot race garbage collection; writer locks still fail.
        if lock.filepath == META_LOCK.filepath && lock.is_blocking == META_LOCK.is_blocking {
            Ok(DirectoryLock::from(Box::new(())))
        } else {
            Err(LockError::wrap_io_error(read_only()))
        }
    }

    fn watch(&self, _callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        Ok(WatchHandle::empty())
    }
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use tantivy::directory::INDEX_WRITER_LOCK;

    static FILES: &[(&str, &[u8])] = &[("meta.json", b"immutable")];

    #[test]
    fn static_component_reads_and_clones_preserve_original_bytes() {
        let directory = EmbeddedDirectory::new(FILES);
        let bytes = directory
            .open_read(Path::new("meta.json"))
            .unwrap()
            .read_bytes()
            .unwrap();
        assert_eq!(bytes.as_ref(), b"immutable");
        assert_eq!(bytes.as_ptr(), FILES[0].1.as_ptr());
        assert_eq!(
            directory
                .clone()
                .atomic_read(Path::new("meta.json"))
                .unwrap(),
            b"immutable"
        );
        assert!(directory.atomic_read(Path::new("../meta.json")).is_err());
    }

    #[test]
    fn only_immutable_metadata_lock_is_allowed() {
        let directory = EmbeddedDirectory::new(FILES);
        assert!(directory.acquire_lock(&META_LOCK).is_ok());
        assert!(directory.acquire_lock(&INDEX_WRITER_LOCK).is_err());
        assert!(directory.open_write(Path::new("new")).is_err());
        assert!(
            directory
                .atomic_write(Path::new("meta.json"), b"replace")
                .is_err()
        );
        assert!(directory.delete(Path::new("meta.json")).is_err());
        assert!(!directory.exists(Path::new("new")).unwrap());
    }
}
