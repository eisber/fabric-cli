// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_command_contracts::CommandError;
use fabric_identity::Binding;
#[cfg(not(windows))]
use std::path::Path;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn config_error() -> CommandError {
    CommandError::new(
        "InvalidConfig",
        "Cannot read or update Rust selection. Repair its permissions/content or run fab-rs auth logout.",
    )
}

/// Independent Rust-specific tenant/principal metadata file; never a token cache.
pub struct SelectionFile {
    path: PathBuf,
}

impl SelectionFile {
    /// Use an explicit caller-owned configuration location.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Discover a platform-specific Rust configuration directory, separate from Python/Azure.
    pub fn discover() -> Result<Self, CommandError> {
        #[cfg(windows)]
        let base = std::env::var_os("APPDATA").map(PathBuf::from);
        #[cfg(not(windows))]
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| Path::new(value).is_absolute())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        let base = base
            .filter(|path| path.is_absolute())
            .ok_or_else(config_error)?;
        Ok(Self::new(base.join("fab-rs").join("selection.json")))
    }

    /// Read bounded strict tenant/principal metadata. Missing means no selection.
    pub fn read(&self) -> Result<Option<Binding>, CommandError> {
        match fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(config_error()),
            Ok(metadata) if !metadata.file_type().is_file() => return Err(config_error()),
            Ok(_) => {}
        }
        let file = File::open(&self.path).map_err(|_| config_error())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file
                .metadata()
                .map_err(|_| config_error())?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err(config_error());
            }
        }
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| config_error())?;
        if bytes.len() > 4096 {
            return Err(config_error());
        }
        let binding: Binding = serde_json::from_slice(&bytes).map_err(|_| config_error())?;
        binding.validate()?;
        Ok(Some(binding))
    }

    /// Atomically replace selection with metadata only, using a private create-new temporary file.
    pub fn write(&self, binding: &Binding) -> Result<(), CommandError> {
        binding.validate()?;
        let directory = self.path.parent().ok_or_else(config_error)?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(directory).map_err(|_| config_error())?;
        let bytes = serde_json::to_vec(binding).map_err(|_| config_error())?;
        let temp = directory.join(format!(
            ".selection-{}-{}.tmp",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|_| config_error())?;
        let result = (|| {
            file.write_all(&bytes).map_err(|_| config_error())?;
            file.sync_all().map_err(|_| config_error())?;
            drop(file);
            fs::rename(&temp, &self.path).map_err(|_| config_error())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }

    /// Clear only Rust selection. Safe when already absent, even if old contents are malformed.
    pub fn clear(&self) -> Result<(), CommandError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(config_error()),
        }
    }
}
