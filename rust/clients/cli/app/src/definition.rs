// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Local definition trees: home expansion, safe part paths, base64 codecs, and atomic writes.

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_cli_command_contracts::CommandError;
use fabric_core::MAX_REQUEST_BYTES;
use serde_json::{Map, Value};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new("InvalidInput", message)
}

fn io_error(action: &str, path: &str) -> CommandError {
    CommandError::new("FileSystem", format!("Cannot {action} {path}"))
}

/// Expand a leading `~` like Python `os.path.expanduser`; `~user` stays literal.
pub(crate) fn expand_home(
    raw: &str,
    home: &dyn Fn() -> Option<PathBuf>,
) -> Result<PathBuf, CommandError> {
    if raw.is_empty() {
        return Err(invalid("Path must not be empty"));
    }
    let rest = if raw == "~" {
        Some("")
    } else if let Some(rest) = raw.strip_prefix("~/") {
        Some(rest)
    } else if cfg!(windows) {
        raw.strip_prefix("~\\")
    } else {
        None
    };
    let Some(rest) = rest else {
        return Ok(PathBuf::from(raw));
    };
    let home = home()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| invalid("Cannot expand ~: home directory is unknown"))?;
    Ok(if rest.is_empty() {
        home
    } else {
        home.join(rest)
    })
}

/// Resolve home like Python `os.path.expanduser`: `HOME` on Unix; on Windows
/// `USERPROFILE`, then `HOMEDRIVE` + `HOMEPATH`. Empty values count as unset.
pub(crate) fn home_from_env(
    windows: bool,
    var: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let get = |name: &str| var(name).filter(|value| !value.is_empty());
    if !windows {
        return get("HOME").map(PathBuf::from);
    }
    get("USERPROFILE").map(PathBuf::from).or_else(|| {
        let mut joined = get("HOMEDRIVE")?;
        joined.push(get("HOMEPATH")?);
        Some(PathBuf::from(joined))
    })
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate one portable path segment that is safe on macOS, Linux, and Windows.
pub(crate) fn validate_segment(segment: &str) -> Result<(), CommandError> {
    let stem = segment.split('.').next().unwrap_or_default();
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment.ends_with(['.', ' '])
        || segment
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'))
        || RESERVED.iter().any(|name| stem.eq_ignore_ascii_case(name))
    {
        return Err(invalid(format!(
            "Unsafe definition path segment: {segment:?}"
        )));
    }
    Ok(())
}

/// Validate a `/`-separated relative part path and return its segments.
pub(crate) fn validate_part_path(path: &str) -> Result<Vec<&str>, CommandError> {
    let segments: Vec<&str> = path.split('/').collect();
    for segment in &segments {
        validate_segment(segment)?;
    }
    Ok(segments)
}

/// Decoded file relative to an output root.
pub(crate) struct LocalFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// Decode service definition parts, optionally stripping a leading `/` (bulk export paths).
pub(crate) fn decode_parts(
    parts: &Value,
    leading_slash: bool,
) -> Result<Vec<LocalFile>, CommandError> {
    let protocol = |message: &str| CommandError::new("Protocol", message);
    let parts = parts
        .as_array()
        .ok_or_else(|| protocol("Definition parts must be an array"))?;
    let mut seen = HashSet::new();
    let mut files = Vec::with_capacity(parts.len());
    for part in parts {
        let path = part
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| protocol("Definition part path is missing"))?;
        let path = if leading_slash {
            path.strip_prefix('/').unwrap_or(path)
        } else {
            path
        };
        validate_part_path(path)?;
        if !seen.insert(path.to_lowercase()) {
            return Err(invalid(format!("Duplicate definition part path: {path}")));
        }
        match part.get("payloadType").and_then(Value::as_str) {
            Some("InlineBase64") => {}
            _ => return Err(protocol("Unsupported definition part payloadType")),
        }
        let payload = part
            .get("payload")
            .and_then(Value::as_str)
            .ok_or_else(|| protocol("Definition part payload is missing"))?;
        let bytes = STANDARD
            .decode(payload)
            .map_err(|_| protocol("Definition part payload is not valid base64"))?;
        files.push(LocalFile {
            path: path.to_owned(),
            bytes,
        });
    }
    Ok(files)
}

/// Require an existing, real output/input directory.
pub(crate) fn require_directory(path: &Path, label: &str) -> Result<(), CommandError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        _ => Err(invalid(format!("{label} must be an existing directory"))),
    }
}

fn check_components(root: &Path, segments: &[&str]) -> Result<Option<fs::Metadata>, CommandError> {
    let mut current = root.to_path_buf();
    for (index, segment) in segments.iter().enumerate() {
        current.push(segment);
        let last = index + 1 == segments.len();
        match fs::symlink_metadata(&current) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(io_error("inspect", &current.display().to_string())),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid(format!(
                    "Refusing to write through symlink {}",
                    current.display()
                )));
            }
            Ok(metadata) if last => {
                if !metadata.is_file() {
                    return Err(invalid(format!(
                        "Destination is not a regular file: {}",
                        current.display()
                    )));
                }
                return Ok(Some(metadata));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(invalid(format!(
                    "Destination is not a directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
        }
    }
    Ok(None)
}

/// Write files below `root` without following symlinks; refuse existing files unless overwriting.
pub(crate) fn write_tree(
    root: &Path,
    prefix: &[&str],
    files: &[LocalFile],
    overwrite: bool,
) -> Result<Vec<String>, CommandError> {
    let mut plans = Vec::with_capacity(files.len());
    for file in files {
        let mut segments = prefix.to_vec();
        segments.extend(validate_part_path(&file.path)?);
        if check_components(root, &segments)?.is_some() && !overwrite {
            return Err(CommandError::new(
                "AlreadyExists",
                format!(
                    "{} exists; pass --overwrite to replace it",
                    segments.join("/")
                ),
            ));
        }
        plans.push(segments);
    }
    let mut written = Vec::with_capacity(files.len());
    for (file, segments) in files.iter().zip(&plans) {
        let mut directory = root.to_path_buf();
        for segment in &segments[..segments.len() - 1] {
            directory.push(segment);
            create_private_directory(&directory)?;
        }
        check_components(root, segments)?;
        let name = segments[segments.len() - 1];
        let temp = directory.join(format!(
            ".{name}-{}-{}.tmp",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let target = directory.join(name);
        let display = target.display().to_string();
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options
            .open(&temp)
            .map_err(|_| io_error("create", &display))?;
        let result = (|| {
            handle
                .write_all(&file.bytes)
                .map_err(|_| io_error("write", &display))?;
            handle.sync_all().map_err(|_| io_error("write", &display))?;
            drop(handle);
            fs::rename(&temp, &target).map_err(|_| io_error("replace", &display))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        written.push(display);
    }
    Ok(written)
}

fn create_private_directory(path: &Path) -> Result<(), CommandError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_dir() => Ok(()),
                _ => Err(invalid(format!(
                    "Destination is not a directory: {}",
                    path.display()
                ))),
            }
        }
        Err(_) => Err(io_error("create directory", &path.display().to_string())),
    }
}

/// Read a definition directory into `InlineBase64` parts with Python CLI parity.
pub(crate) fn read_tree(root: &Path) -> Result<Vec<Value>, CommandError> {
    let mut relative = Vec::new();
    walk(root, &mut Vec::new(), &mut relative)?;
    let mut parts = Vec::with_capacity(relative.len());
    let mut total = 0usize;
    let mut seen = HashSet::new();
    for segments in relative {
        let path = segments.join("/");
        if path.contains("cache.abf") {
            continue;
        }
        validate_part_path(&path)?;
        if !seen.insert(path.to_lowercase()) {
            return Err(invalid(format!("Duplicate definition part path: {path}")));
        }
        let full = segments.iter().fold(root.to_path_buf(), |p, s| p.join(s));
        let bytes = read_bounded(&full, &path, MAX_REQUEST_BYTES - total)?;
        if segments
            .last()
            .is_some_and(|name| *name == "definition.pbir")
        {
            let by_path = fabric_core_json::decode(&bytes)
                .map_err(|_| invalid(format!("{path} is not valid JSON")))?
                .get("datasetReference")
                .and_then(|reference| reference.get("byPath"))
                .is_some_and(|value| !value.is_null());
            if by_path {
                return Err(CommandError::new(
                    "InvalidDefinitionPayload",
                    "Definition includes byPath; switch to byConnection before importing",
                ));
            }
        }
        let payload = STANDARD.encode(&bytes);
        total = total.saturating_add(payload.len() + path.len());
        if total > MAX_REQUEST_BYTES {
            return Err(invalid("Definition exceeds request allocation bound"));
        }
        let mut part = Map::new();
        part.insert("path".into(), Value::String(path));
        part.insert("payload".into(), Value::String(payload));
        part.insert("payloadType".into(), Value::String("InlineBase64".into()));
        parts.push(Value::Object(part));
    }
    if parts.is_empty() {
        return Err(invalid("Source directory contains no definition files"));
    }
    Ok(parts)
}

fn walk(
    directory: &Path,
    prefix: &mut Vec<String>,
    out: &mut Vec<Vec<String>>,
) -> Result<(), CommandError> {
    let display = directory.display().to_string();
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(|_| io_error("read directory", &display))? {
        let entry = entry.map_err(|_| io_error("read directory", &display))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("Definition file names must be UTF-8"))?;
        let kind = entry
            .file_type()
            .map_err(|_| io_error("inspect", &entry.path().display().to_string()))?;
        entries.push((name, kind));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let (files, directories): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|(_, kind)| !kind.is_dir() || kind.is_symlink());
    for (name, kind) in files {
        if !kind.is_file() {
            return Err(invalid(format!(
                "Source contains a symlink or special file: {}",
                directory.join(&name).display()
            )));
        }
        let mut path = prefix.clone();
        path.push(name);
        out.push(path);
    }
    for (name, _) in directories {
        prefix.push(name.clone());
        walk(&directory.join(name), prefix, out)?;
        prefix.pop();
    }
    Ok(())
}

fn read_bounded(path: &Path, label: &str, limit: usize) -> Result<Vec<u8>, CommandError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|_| io_error("open", label))?;
    if !file
        .metadata()
        .map_err(|_| io_error("inspect", label))?
        .is_file()
    {
        return Err(invalid(format!("{label} is not a regular file")));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| io_error("read", label))?;
    if bytes.len() > limit {
        return Err(invalid("Definition exceeds request allocation bound"));
    }
    Ok(bytes)
}

/// Item type from a `.platform` part (`metadata.type`), if present.
pub(crate) fn platform_type(parts: &[Value]) -> Result<Option<String>, CommandError> {
    let Some(part) = parts
        .iter()
        .find(|part| part.get("path").and_then(Value::as_str) == Some(".platform"))
    else {
        return Ok(None);
    };
    let bytes = part
        .get("payload")
        .and_then(Value::as_str)
        .and_then(|payload| STANDARD.decode(payload).ok())
        .ok_or_else(|| invalid(".platform cannot be decoded"))?;
    let value =
        fabric_core_json::decode(&bytes).map_err(|_| invalid(".platform is not valid JSON"))?;
    Ok(value
        .get("metadata")
        .and_then(|metadata| metadata.get("type"))
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_segments() {
        for path in [
            "", "a//b", "../x", "a/./b", "a\\b", "C:x", "nul.txt", "a/b.", "a\u{1}",
        ] {
            assert!(validate_part_path(path).is_err(), "{path}");
        }
        assert!(validate_part_path("Folder/My Report.Report/.platform").is_ok());
    }

    #[test]
    fn expands_home_from_host() {
        let base = std::env::temp_dir();
        let home = || Some(base.clone());
        assert_eq!(expand_home("~", &home).unwrap(), base);
        assert_eq!(expand_home("~/a/b", &home).unwrap(), base.join("a/b"));
        assert_eq!(
            expand_home("~other/x", &home).unwrap(),
            PathBuf::from("~other/x")
        );
        assert_eq!(expand_home("rel", &home).unwrap(), PathBuf::from("rel"));
        let absolute = base.join("abs");
        assert_eq!(
            expand_home(absolute.to_str().unwrap(), &home).unwrap(),
            absolute
        );
        assert!(expand_home("~", &|| None).is_err());
        assert!(expand_home("~/x", &|| Some(PathBuf::from("relative"))).is_err());
        assert!(expand_home("", &home).is_err());
    }

    #[test]
    fn resolves_home_like_python() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        assert_eq!(
            home_from_env(false, env(&[("HOME", "/home/u"), ("USERPROFILE", "/x")])),
            Some(PathBuf::from("/home/u"))
        );
        assert_eq!(home_from_env(false, env(&[("HOME", "")])), None);
        assert_eq!(
            home_from_env(true, env(&[("USERPROFILE", r"C:\Users\u"), ("HOME", "/h")])),
            Some(PathBuf::from(r"C:\Users\u"))
        );
        assert_eq!(
            home_from_env(
                true,
                env(&[
                    ("USERPROFILE", ""),
                    ("HOMEDRIVE", "D:"),
                    ("HOMEPATH", r"\Users\u")
                ])
            ),
            Some(PathBuf::from(r"D:\Users\u"))
        );
        assert_eq!(home_from_env(true, env(&[("HOMEDRIVE", "D:")])), None);
        assert_eq!(home_from_env(true, env(&[("HOME", "/h")])), None);
    }
}
