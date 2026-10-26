// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{DefinitionFile, DefinitionFileSystem, DefinitionLimits, validate_part_path};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf, Prefix},
};

/// Explicit local adapter for stable, caller-owned output directories.
///
/// No current directory/home is discovered. Existing ordinary files are replaced
/// without another confirmation, as in Python export. Symlink/reparse/network/
/// device paths and portable aliases fail. This does not protect against hostile
/// concurrent directory replacement; the caller must own the output tree.
pub struct LocalDefinitionFileSystem {
    limits: DefinitionLimits,
}

impl LocalDefinitionFileSystem {
    /// Configure finite traversal bounds without performing I/O.
    pub const fn new(limits: DefinitionLimits) -> Self {
        Self { limits }
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "unsafe definition output path")
}

fn regular(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    metadata.is_dir() || metadata.is_file()
}

fn directory_chain(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(invalid());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        if component == Component::ParentDir {
            return Err(invalid());
        }
        if let Component::Prefix(prefix) = component {
            if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
                return Err(invalid());
            }
            current.push(component);
            continue;
        }
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() || !regular(&metadata) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn check_case_alias(parent: &Path, name: &str, limits: DefinitionLimits) -> io::Result<()> {
    for (count, entry) in fs::read_dir(parent)?.enumerate() {
        if count >= limits.max_files.get() {
            return Err(invalid());
        }
        let existing = entry?.file_name();
        if existing.to_string_lossy().to_lowercase() == name.to_lowercase()
            && existing.to_str() != Some(name)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "case-aliased definition output path",
            ));
        }
    }
    Ok(())
}

fn ensure_directory(root: &Path, relative: &str, limits: DefinitionLimits) -> io::Result<PathBuf> {
    directory_chain(root)?;
    validate_part_path(relative, limits).map_err(|_| invalid())?;
    let mut current = root.to_path_buf();
    for component in relative.split('/') {
        check_case_alias(&current, component, limits)?;
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || !regular(&metadata) {
                    return Err(invalid());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(current)
}

impl DefinitionFileSystem for LocalDefinitionFileSystem {
    fn preflight(&mut self, root: &Path, files: &[DefinitionFile]) -> io::Result<()> {
        directory_chain(root)?;
        for file in files {
            validate_part_path(&file.path, self.limits).map_err(|_| invalid())?;
            let mut current = root.to_path_buf();
            let parts: Vec<_> = file.path.split('/').collect();
            for (index, part) in parts.iter().enumerate() {
                if current.is_dir() {
                    check_case_alias(&current, part, self.limits)?;
                }
                current.push(part);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => {
                        if !regular(&metadata) || (index + 1 == parts.len()) != metadata.is_file() {
                            return Err(invalid());
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    fn create_directory(&mut self, root: &Path, item_name: &str) -> io::Result<()> {
        directory_chain(root)?;
        validate_part_path(item_name, self.limits).map_err(|_| invalid())?;
        if item_name.contains('/') {
            return Err(invalid());
        }
        ensure_directory(root, item_name, self.limits).map(|_| ())
    }

    fn write(&mut self, root: &Path, file: &DefinitionFile) -> io::Result<()> {
        directory_chain(root)?;
        validate_part_path(&file.path, self.limits).map_err(|_| invalid())?;
        let path = file
            .path
            .split('/')
            .fold(root.to_path_buf(), |path, part| path.join(part));
        if let Some((parent, name)) = file.path.rsplit_once('/') {
            let parent = ensure_directory(root, parent, self.limits)?;
            check_case_alias(&parent, name, self.limits)?;
        } else {
            check_case_alias(root, &file.path, self.limits)?;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() || !regular(&metadata) => {
                return Err(invalid());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        output.write_all(&file.bytes)?;
        output.flush()
    }
}
