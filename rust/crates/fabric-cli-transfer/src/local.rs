// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{TransferFiles, TransferFuture};
use fabric_deploy::definitions::{
    DefinitionEntry, DefinitionFile, DefinitionFileSystem, DefinitionImportFileSystem,
    DefinitionLimits, LocalDefinitionFileSystem,
};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Local CLI adapter confined to one explicit, stable caller-owned root.
///
/// The caller resolves the root before construction, including temporary-root
/// system aliases. Individual service paths are never canonicalized to bypass
/// the released library's link/reparse/alias checks. No home or cwd is discovered.
pub struct LocalTransferFiles {
    root: PathBuf,
    home: Option<PathBuf>,
    definitions: LocalDefinitionFileSystem,
}

impl LocalTransferFiles {
    /// Validate an existing owned root and optional explicit tilde expansion.
    ///
    /// The home directory must be inside the owned root. This is a containment
    /// policy for trusted stable trees, not a hostile-concurrent-filesystem sandbox.
    pub fn new(root: PathBuf, home: Option<PathBuf>, limits: DefinitionLimits) -> io::Result<Self> {
        let mut definitions = LocalDefinitionFileSystem::new(limits);
        definitions.preflight(&root, &[])?;
        if let Some(home) = &home {
            if !home.starts_with(&root) {
                return Err(invalid());
            }
            definitions.preflight(home, &[])?;
        }
        Ok(Self {
            root,
            home,
            definitions,
        })
    }

    fn check(&mut self, root: &Path) -> io::Result<()> {
        if !root.starts_with(&self.root) {
            return Err(invalid());
        }
        self.definitions.preflight(root, &[])
    }

    fn check_import(&mut self, root: &Path) -> io::Result<()> {
        if !root.starts_with(&self.root) {
            return Err(invalid());
        }
        if root == self.root.as_path() {
            return self.check(root);
        }
        let parent = root.parent().ok_or_else(invalid)?;
        self.check(parent)?;
        let metadata = fs::symlink_metadata(root)?;
        if metadata.is_file() {
            let name = root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(invalid)?;
            self.definitions.preflight(
                parent,
                &[DefinitionFile {
                    path: name.to_owned(),
                    bytes: Vec::new(),
                }],
            )
        } else {
            self.check(root)
        }
    }

    fn selected_path(&self, input: &str) -> io::Result<PathBuf> {
        if input.is_empty() || input.contains("://") {
            return Err(invalid());
        }
        let path = if input == "~" {
            self.home.clone().ok_or_else(invalid)?
        } else if let Some(suffix) = input
            .strip_prefix("~/")
            .or_else(|| input.strip_prefix("~\\"))
        {
            self.home.as_ref().ok_or_else(invalid)?.join(suffix)
        } else {
            let path = Path::new(input);
            if path.is_absolute() {
                path.to_owned()
            } else {
                self.root.join(path)
            }
        };
        if path.components().any(|part| part == Component::ParentDir) {
            return Err(invalid());
        }
        Ok(path)
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Local transfer path is outside the owned root or is unsafe",
    )
}

impl TransferFiles for LocalTransferFiles {
    fn resolve_directory(&mut self, input: &str) -> io::Result<PathBuf> {
        let path = self.selected_path(input)?;
        self.check(&path)?;
        Ok(path)
    }

    fn resolve_import(&mut self, input: &str) -> io::Result<PathBuf> {
        let path = self.selected_path(input)?;
        self.check_import(&path)?;
        Ok(path)
    }

    fn resolve_import_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        Box::pin(async move { self.resolve_import(input) })
    }

    fn is_empty(&mut self, root: &Path) -> io::Result<bool> {
        self.check(root)?;
        Ok(self.definitions.entries(root, "")?.is_empty())
    }
}

impl DefinitionFileSystem for LocalTransferFiles {
    fn preflight(&mut self, root: &Path, files: &[DefinitionFile]) -> io::Result<()> {
        self.check(root)?;
        self.definitions.preflight(root, files)
    }

    fn create_directory(&mut self, root: &Path, name: &str) -> io::Result<()> {
        self.check(root)?;
        self.definitions.create_directory(root, name)
    }

    fn write(&mut self, root: &Path, file: &DefinitionFile) -> io::Result<()> {
        self.check(root)?;
        self.definitions.write(root, file)
    }
}

impl DefinitionImportFileSystem for LocalTransferFiles {
    fn entries(&mut self, root: &Path, relative: &str) -> io::Result<Vec<DefinitionEntry>> {
        self.check_import(root)?;
        self.definitions.entries(root, relative)
    }

    fn read(&mut self, root: &Path, relative: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        self.check(root)?;
        self.definitions.read(root, relative, max_bytes)
    }
}
