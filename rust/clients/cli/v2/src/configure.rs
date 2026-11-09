// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit non-secret preferences, separate from the v1 configuration store.
//!
//! Validated writes use a bounded, synced sibling file followed by atomic
//! replacement. An OS-held lock excludes cooperating writers and is released on
//! process termination. The persistent lock file is not a stale-lock sentinel.
//! Existing symlinks, Windows reparse points, and nonregular preference files are
//! rejected. The explicitly selected directory must remain caller-controlled;
//! this is not a sandbox against another process with the same filesystem rights.
//! Filesystem-specific power-loss durability is not an installation guarantee.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value, json};

use crate::{CliError, Control, EffectKind, EffectState, OperationResult, check_control};

/// Separate v2 preference filename beneath the explicitly selected root.
pub const PREFERENCES_FILE: &str = "preferences-v2.json";

const LOCK_FILE: &str = ".preferences-v2.lock";
const MAX_BYTES: usize = 64 * 1024;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

/// Read a bounded snapshot of explicit v2 preferences without initializing state.
///
/// A missing root or preference file is an empty map. This does not create
/// directories, open the writer lock, migrate legacy keys, or insert defaults.
/// Recognized preferences must have the exact advertised string values.
/// Unrelated members are preserved as data, not account/profile/target authority.
/// Concurrent atomic replacement yields the old or new opened-file snapshot.
///
/// # Errors
/// Rejects relative/traversing roots, linked/reparse/nonregular paths, malformed
/// or oversized JSON, invalid recognized preferences, and filesystem failures.
pub fn read(root: &Path) -> Result<Map<String, Value>, CliError> {
    validate_root(root)?;
    let mut directories: Vec<PathBuf> = root.ancestors().map(Path::to_path_buf).collect();
    directories.reverse();
    for path in &directories {
        match fs::symlink_metadata(path) {
            Ok(metadata) => check_metadata(&metadata, true)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(_) => return Err(io_error("The preference directory could not be inspected.")),
        }
    }
    let _directory_guards = hold_directories(&directories)?;
    check_directories(&directories)?;
    let settings = read_settings(&root.join(PREFERENCES_FILE))?;
    for (key, value) in &settings {
        if let Some(allowed) = allowed_values(key)
            && !value.as_str().is_some_and(|value| allowed.contains(&value))
        {
            return Err(CliError::new(
                "InvalidConfig",
                "A recognized v2 preference has an invalid value or type.",
            ));
        }
    }
    Ok(settings)
}

/// Set one explicitly supported tool preference, never a remote Fabric setting.
///
/// The root must be absolute. Keys, values and cancellation are checked before
/// filesystem access. Existing unrelated JSON members are retained without
/// migration or default insertion. The preference effect is completed only after
/// replacement; errors after replacement retain that evidence. No credentials,
/// environment defaults, profile discovery, or network operations are used.
///
/// # Errors
/// Rejects invalid targets/values, unsafe paths, oversized/malformed stores,
/// concurrent writers, cancellation/deadlines and filesystem failures.
pub fn set(
    root: &Path,
    target: &str,
    value: &str,
    control: &impl Control,
) -> Result<OperationResult, CliError> {
    let key = validate(root, target, value)?;
    check_control(control)?;
    let directories = prepare_directories(root, control)?;
    let _directory_guards = hold_directories(&directories)?;
    let lock_path = root.join(LOCK_FILE);
    check_file(&lock_path)?;
    let mut options = private_options();
    options.read(true).write(true).create(true).truncate(false);
    let lock = options
        .open(&lock_path)
        .map_err(|_| io_error("The preference writer lock could not be opened."))?;
    check_metadata(
        &lock
            .metadata()
            .map_err(|_| io_error("The writer lock could not be inspected."))?,
        false,
    )?;
    check_file(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(CliError::new(
                "ConfigurationBusy",
                "Another writer holds the v2 preference lock.",
            ));
        }
        Err(std::fs::TryLockError::Error(_)) => {
            return Err(io_error(
                "The filesystem could not lock the preference store.",
            ));
        }
    }
    check_control(control)?;
    check_directories(&directories)?;
    let destination = root.join(PREFERENCES_FILE);
    let mut settings = read_settings(&destination)?;
    settings.insert(key.to_owned(), Value::String(value.to_owned()));
    let bytes = serde_json::to_vec_pretty(&Value::Object(settings))
        .map_err(|_| CliError::new("InvalidConfig", "The preferences could not be encoded."))?;
    if bytes.len() > MAX_BYTES {
        return Err(CliError::new(
            "BudgetExceeded",
            "The encoded preferences exceed the 64 KiB store limit.",
        ));
    }
    check_control(control)?;
    let mut pending = PendingFile::create(root)?;
    let file = pending.file.as_mut().expect("pending file is open");
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| io_error("The replacement preferences could not be written and synced."))?;
    check_control(control)?;
    check_directories(&directories)?;
    check_file(&destination)?;
    // Close before replacement so Windows never needs delete sharing on the
    // writable file. Creation was exclusive and the root is caller-owned.
    drop(pending.file.take());
    fs::rename(&pending.path, &destination).map_err(|_| {
        io_error("Preference replacement failed; inspect the selected store before retrying.")
            .with_effect(EffectKind::ConfigurationWrite, EffectState::Unknown)
    })?;
    pending.committed = true;
    sync_directory(root).map_err(|error| {
        error.with_effect(EffectKind::ConfigurationWrite, EffectState::Completed)
    })?;
    check_control(control).map_err(|error| {
        error.with_effect(EffectKind::ConfigurationWrite, EffectState::Completed)
    })?;
    Ok(OperationResult::succeeded(json!({
        "target": target,
        "key": key,
        "value": value,
        "store": PREFERENCES_FILE,
    }))
    .with_effect(EffectKind::ConfigurationWrite, EffectState::Completed))
}

fn validate<'a>(root: &Path, target: &'a str, value: &str) -> Result<&'a str, CliError> {
    validate_root(root)?;
    let key = target.strip_prefix("/.config/").ok_or_else(|| {
        CliError::new(
            "UnsupportedTarget",
            "Only an explicit /.config/<key> target is supported.",
        )
    })?;
    let allowed = allowed_values(key).ok_or_else(|| {
        CliError::new(
            "UnsupportedSetting",
            "The setting is not an admitted v2 tool preference.",
        )
    })?;
    if !allowed.contains(&value) {
        return Err(CliError::new(
            "InvalidInput",
            "The value must exactly match an advertised preference value.",
        ));
    }
    Ok(key)
}

fn validate_root(root: &Path) -> Result<(), CliError> {
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(CliError::new(
            "InvalidInput",
            "An absolute preference root without parent traversal is required.",
        ));
    }
    Ok(())
}

fn allowed_values(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "output_format" => Some(&["text", "json"]),
        "default_open_experience" => Some(&["fabric", "powerbi"]),
        "output_item_sort_criteria" => Some(&["byname", "bytype"]),
        "show_hidden" | "folder_listing_enabled" | "debug_enabled" | "cache_enabled" => {
            Some(&["true", "false"])
        }
        _ => None,
    }
}

fn io_error(message: &'static str) -> CliError {
    CliError::new("LocalIOError", message)
}

fn check_metadata(metadata: &fs::Metadata, directory: bool) -> Result<(), CliError> {
    let mut unsafe_path = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        unsafe_path |= metadata.file_attributes() & 0x400 != 0;
    }
    #[cfg(unix)]
    if !directory {
        use std::os::unix::fs::MetadataExt;
        unsafe_path |= metadata.nlink() != 1;
    }
    if unsafe_path || (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(CliError::new(
            "UnsafePath",
            "Preferences require ordinary directories and nonlinked regular files.",
        ));
    }
    Ok(())
}

fn check_file(path: &Path) -> Result<(), CliError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => check_metadata(&metadata, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(io_error("The preference path could not be inspected.")),
    }
}

fn prepare_directories(root: &Path, control: &impl Control) -> Result<Vec<PathBuf>, CliError> {
    let mut directories: Vec<PathBuf> = root.ancestors().map(Path::to_path_buf).collect();
    directories.reverse();
    for path in &directories {
        check_control(control)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) => check_metadata(&metadata, true)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(unix)]
                let builder = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = fs::DirBuilder::new();
                    builder.mode(0o700);
                    builder
                };
                #[cfg(not(unix))]
                let builder = fs::DirBuilder::new();
                match builder.create(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => {
                        return Err(io_error("The preference directory could not be created."));
                    }
                }
                check_metadata(
                    &fs::symlink_metadata(path)
                        .map_err(|_| io_error("The created directory could not be inspected."))?,
                    true,
                )?;
            }
            Err(_) => return Err(io_error("The preference directory could not be inspected.")),
        }
    }
    Ok(directories)
}

fn check_directories(directories: &[PathBuf]) -> Result<(), CliError> {
    for path in directories {
        check_metadata(
            &fs::symlink_metadata(path)
                .map_err(|_| io_error("The selected directory is no longer available."))?,
            true,
        )?;
    }
    Ok(())
}

// Keep Windows ancestors from being renamed/replaced during the operation.
// Unix callers retain ownership of the directory namespace, as documented above.
fn hold_directories(directories: &[PathBuf]) -> Result<Vec<File>, CliError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        directories
            .iter()
            .map(|path| {
                let file = OpenOptions::new()
                    .read(true)
                    .share_mode(0x1 | 0x2)
                    .custom_flags(0x02000000 | 0x00200000)
                    .open(path)
                    .map_err(|_| io_error("The preference directory could not be held safely."))?;
                check_metadata(
                    &file
                        .metadata()
                        .map_err(|_| io_error("The held directory could not be inspected."))?,
                    true,
                )?;
                Ok(file)
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        let _ = directories;
        Ok(Vec::new())
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x1 | 0x2).custom_flags(0x00200000);
    }
    options
}

fn read_settings(path: &Path) -> Result<Map<String, Value>, CliError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => check_snapshot_metadata(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(_) => return Err(io_error("The preference path could not be inspected.")),
    }
    let mut options = private_options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // An open read snapshot must not prevent atomic replacement by a writer.
        options.share_mode(0x1 | 0x2 | 0x4);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(_) => return Err(io_error("The v2 preferences could not be read.")),
    };
    check_snapshot_metadata(
        &file
            .metadata()
            .map_err(|_| io_error("The opened preferences could not be inspected."))?,
    )?;
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| io_error("The v2 preferences could not be read."))?;
    if bytes.len() > MAX_BYTES {
        return Err(CliError::new(
            "BudgetExceeded",
            "The existing preferences exceed the 64 KiB store limit.",
        ));
    }
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(settings)) => Ok(settings),
        _ => Err(CliError::new(
            "InvalidConfig",
            "Existing v2 preferences must contain a valid UTF-8 JSON object.",
        )),
    }
}

fn check_snapshot_metadata(metadata: &fs::Metadata) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Atomic replacement can unlink the old inode during stat or after it
        // was opened. It remains a snapshot, unlike an extra hard-linked alias.
        if metadata.is_file() && metadata.nlink() == 0 {
            return Ok(());
        }
    }
    check_metadata(metadata, false)
}

fn sync_directory(root: &Path) -> Result<(), CliError> {
    #[cfg(unix)]
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| {
            io_error("Preferences were replaced, but directory durability is unconfirmed.")
        })?;
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

struct PendingFile {
    file: Option<File>,
    path: PathBuf,
    committed: bool,
}

impl PendingFile {
    fn create(root: &Path) -> Result<Self, CliError> {
        for _ in 0..16 {
            let path = root.join(format!(
                ".preferences-v2-{}-{}.pending",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed),
            ));
            match private_options().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        file: Some(file),
                        path,
                        committed: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => {
                    return Err(io_error(
                        "The replacement preference file could not be created.",
                    ));
                }
            }
        }
        Err(io_error(
            "No exclusive replacement preference filename is available.",
        ))
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
