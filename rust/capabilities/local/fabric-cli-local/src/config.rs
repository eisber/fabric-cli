// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fabric_cli_output::json_dumps;
use serde_json::{Map, Value};

use crate::LocalError;

/// One Python configuration key, in source listing order.
#[derive(Clone, Copy, Debug)]
pub struct Setting {
    /// Exact persisted key.
    pub key: &'static str,
    /// Case-sensitive accepted values; an empty slice permits any string.
    pub allowed: &'static [&'static str],
    /// Initial value, absent for settings requiring user input.
    pub default: Option<&'static str>,
}

const BOOL: &[&str] = &["false", "true"];

/// Configuration keys from Python `core/fab_constant.py` at the pinned source.
pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "cache_enabled",
        allowed: BOOL,
        default: Some("true"),
    },
    Setting {
        key: "context_persistence_enabled",
        allowed: BOOL,
        default: Some("false"),
    },
    Setting {
        key: "debug_enabled",
        allowed: BOOL,
        default: Some("false"),
    },
    Setting {
        key: "encryption_fallback_enabled",
        allowed: BOOL,
        default: Some("false"),
    },
    Setting {
        key: "job_cancel_ontimeout",
        allowed: BOOL,
        default: Some("true"),
    },
    Setting {
        key: "local_definition_labels",
        allowed: &[],
        default: None,
    },
    Setting {
        key: "output_item_sort_criteria",
        allowed: &["byname", "bytype"],
        default: Some("byname"),
    },
    Setting {
        key: "show_hidden",
        allowed: BOOL,
        default: Some("false"),
    },
    Setting {
        key: "default_az_subscription_id",
        allowed: &[],
        default: None,
    },
    Setting {
        key: "default_az_resource_group",
        allowed: &[],
        default: None,
    },
    Setting {
        key: "default_az_location",
        allowed: &[],
        default: None,
    },
    Setting {
        key: "default_az_admin",
        allowed: &[],
        default: None,
    },
    Setting {
        key: "default_capacity",
        allowed: &[],
        default: None,
    },
    Setting {
        key: "default_open_experience",
        allowed: &["fabric", "powerbi"],
        default: Some("fabric"),
    },
    Setting {
        key: "output_format",
        allowed: &["text", "json"],
        default: Some("text"),
    },
    Setting {
        key: "folder_listing_enabled",
        allowed: BOOL,
        default: Some("false"),
    },
    Setting {
        key: "workspace_private_links_enabled",
        allowed: BOOL,
        default: Some("false"),
    },
    Setting {
        key: "check_cli_version_updates",
        allowed: BOOL,
        default: Some("true"),
    },
];

/// File-backed non-secret CLI settings under an explicit caller-owned directory.
///
/// Construction performs no I/O and never discovers HOME, credentials, or caches.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    root: PathBuf,
}

impl ConfigStore {
    /// Bind the directory that will contain `config.json`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Borrow the injected directory, not an ambient user profile.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read settings, treating only a missing file as an empty configuration.
    ///
    /// Unlike Python's corrupt-JSON fallback, malformed configuration fails
    /// explicitly, avoiding silent loss of existing user settings.
    pub fn read(&self) -> Result<Map<String, Value>, LocalError> {
        let content = match fs::read_to_string(self.root.join("config.json")) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(error.into()),
        };
        match serde_json::from_str(&content) {
            Ok(Value::Object(settings)) => Ok(settings),
            _ => Err(LocalError::InvalidConfig),
        }
    }

    /// Apply source default/migration rules and persist only when changed.
    pub fn initialize(&self) -> Result<Map<String, Value>, LocalError> {
        let mut settings = self.read()?;
        let original = settings.clone();
        settings.shift_remove("mode");
        for setting in SETTINGS {
            let old = format!("fab_{}", setting.key);
            if let Some(value) = settings.shift_remove(&old)
                && !settings.contains_key(setting.key)
            {
                settings.insert(setting.key.to_owned(), value);
            }
            if !settings.contains_key(setting.key)
                && let Some(value) = setting.default
            {
                settings.insert(setting.key.to_owned(), Value::String(value.to_owned()));
            }
        }
        if settings != original {
            self.write(&settings)?;
        }
        Ok(settings)
    }

    /// Persist ordered Python four-space, ASCII JSON without a trailing newline.
    ///
    /// On Unix the directory and file are restricted to owner-only access before
    /// truncation, matching the source. Windows ACL policy belongs to the host.
    pub fn write(&self, settings: &Map<String, Value>) -> Result<(), LocalError> {
        let content = pretty_json(&Value::Object(settings.clone()), 0)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700).create(&self.root)?;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&self.root)?;
        let path = self.root.join("config.json");
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            match fs::metadata(&path) {
                Ok(_) => fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(content.as_bytes())?;
        Ok(())
    }
}

pub(crate) fn pretty_json(value: &Value, depth: usize) -> Result<String, LocalError> {
    let rows = match value {
        Value::Object(object) if !object.is_empty() => object
            .iter()
            .map(|(key, value)| {
                Ok(format!(
                    "{}: {}",
                    json_dumps(&Value::String(key.clone()))?,
                    pretty_json(value, depth + 1)?
                ))
            })
            .collect::<Result<Vec<_>, LocalError>>()?,
        Value::Array(array) if !array.is_empty() => array
            .iter()
            .map(|value| pretty_json(value, depth + 1))
            .collect::<Result<Vec<_>, LocalError>>()?,
        _ => return Ok(json_dumps(value)?),
    };
    let (open, close) = if value.is_object() {
        ('{', '}')
    } else {
        ('[', ']')
    };
    let indent = "    ".repeat(depth + 1);
    Ok(format!(
        "{open}\n{indent}{}\n{}{close}",
        rows.join(&format!(",\n{indent}")),
        "    ".repeat(depth)
    ))
}

pub(crate) fn setting(key: &str) -> Result<&'static Setting, LocalError> {
    SETTINGS.iter().find(|setting| setting.key == key).ok_or_else(|| {
        LocalError::command("InvalidInput", format!("'{key}' is not a recognized configuration key. Please check the available configuration keys"))
    })
}
