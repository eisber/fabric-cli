// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Application-owned non-secret configuration paths; libraries still use injected stores.

use fabric_cli_local::LocalError;
use std::{ffi::OsString, path::PathBuf};

fn absolute(value: OsString) -> Result<PathBuf, LocalError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(LocalError::command(
            "InvalidInput",
            "The configuration root must be absolute",
        ))
    }
}

fn select(
    explicit: Option<OsString>,
    primary: Option<OsString>,
    home: Option<OsString>,
) -> Result<Option<PathBuf>, LocalError> {
    if let Some(explicit) = explicit {
        return absolute(explicit).map(Some);
    }
    if let Some(primary) = primary {
        let root = absolute(primary)?;
        #[cfg(windows)]
        return Ok(Some(root.join("Microsoft").join("FabricCLI")));
        #[cfg(not(windows))]
        return Ok(Some(root.join("microsoft").join("fabric-cli")));
    }
    #[cfg(windows)]
    {
        let _ = home;
        Ok(None)
    }
    #[cfg(not(windows))]
    {
        home.map(absolute).transpose().map(|home| {
            home.map(|home| {
                #[cfg(target_os = "macos")]
                {
                    home.join("Library")
                        .join("Application Support")
                        .join("Microsoft")
                        .join("FabricCLI")
                }
                #[cfg(not(target_os = "macos"))]
                {
                    home.join(".config").join("microsoft").join("fabric-cli")
                }
            })
        })
    }
}

pub(crate) fn available_root() -> Result<Option<PathBuf>, LocalError> {
    select(
        std::env::var_os("FAB_RUST_CONFIG_ROOT"),
        {
            #[cfg(windows)]
            {
                std::env::var_os("LOCALAPPDATA")
            }
            #[cfg(not(windows))]
            {
                std::env::var_os("XDG_CONFIG_HOME")
            }
        },
        {
            #[cfg(windows)]
            {
                None
            }
            #[cfg(not(windows))]
            {
                std::env::var_os("HOME")
            }
        },
    )
}

pub(crate) fn root() -> Result<PathBuf, LocalError> {
    available_root()?.ok_or_else(|| {
        LocalError::command(
            "NotConfigured",
            "No user configuration directory is available. Set FAB_RUST_CONFIG_ROOT to an absolute directory",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_root() -> PathBuf {
        std::env::temp_dir().join("fabric-config-selector")
    }

    #[test]
    fn explicit_root_wins_even_when_profile_roots_are_invalid() {
        let root = absolute_root();
        assert_eq!(
            select(
                Some(root.clone().into()),
                Some("relative".into()),
                Some("relative".into())
            )
            .unwrap(),
            Some(root),
        );
    }

    #[test]
    fn invalid_explicit_or_profile_values_never_silently_fall_back() {
        for value in ["", "relative"] {
            assert!(select(Some(value.into()), Some(absolute_root().into()), None).is_err());
            assert!(select(None, Some(value.into()), Some(absolute_root().into())).is_err());
        }
        assert_eq!(select(None, None, None).unwrap(), None);
    }

    #[test]
    fn default_profile_directory_matches_the_application_binding_root() {
        let root = absolute_root();
        #[cfg(windows)]
        let expected = root.join("Microsoft").join("FabricCLI");
        #[cfg(not(windows))]
        let expected = root.join("microsoft").join("fabric-cli");
        assert_eq!(
            select(None, Some(root.into()), None).unwrap(),
            Some(expected)
        );
    }
}
