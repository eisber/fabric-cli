// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::TransferFiles;
use fabric_deploy::definitions::DefinitionImportFileSystem;
use std::{io, path::Path};

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
