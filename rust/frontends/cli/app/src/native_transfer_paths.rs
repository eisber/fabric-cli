// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! CLI-owned lexical Windows source selection, without filesystem activation.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub(super) fn home_from_environment() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOMEPATH").map(|path| {
                PathBuf::from(std::env::var_os("HOMEDRIVE").unwrap_or_default()).join(path)
            })
        })
}

pub(super) fn candidate(
    input: &str,
    current: &Path,
    home: Option<&Path>,
    username: Option<&OsStr>,
) -> PathBuf {
    let expanded = if input.starts_with('~')
        && let Some(home) = home
    {
        let end = input.find(['/', '\\']).unwrap_or(input.len());
        let requested_user = &input[1..end];
        let selected_home =
            if requested_user.is_empty() || username == Some(OsStr::new(requested_user)) {
                Some(home.to_owned())
            } else if username.is_some()
                && home.file_name() == username
                && !home
                    .as_os_str()
                    .as_encoded_bytes()
                    .last()
                    .is_some_and(|last| matches!(last, b'/' | b'\\'))
            {
                home.parent().map(|parent| parent.join(requested_user))
            } else {
                None
            };
        selected_home.map(|home| home.join(input[end..].trim_start_matches(['/', '\\'])))
    } else {
        None
    };
    let path = expanded.unwrap_or_else(|| PathBuf::from(input));
    if path.is_absolute() {
        path
    } else {
        current.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_fixture_path(value: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(value)
        } else {
            // Retain every lexical case while using the current host's path grammar.
            let value = value
                .strip_prefix("C:")
                .or_else(|| value.strip_prefix("D:"))
                .unwrap_or(value);
            PathBuf::from(value.replace('\\', "/"))
        }
    }

    #[test]
    fn fixture_roots_and_relative_inputs_keep_their_path_classification() {
        assert!(native_fixture_path(r"C:\work").is_absolute());
        assert!(native_fixture_path(r"D:\outside\input").is_absolute());
        assert!(!native_fixture_path(r"..\input").is_absolute());
    }

    #[test]
    fn explicit_candidates_preserve_absolute_roots_and_defer_path_validation() {
        let current = native_fixture_path(r"C:\work");
        for (input, expected) in [
            ("input", r"C:\work\input"),
            (r"D:\outside\input", r"D:\outside\input"),
            (r"..\input", r"C:\work\..\input"),
            (r"\input", r"C:\input"),
        ] {
            let input = native_fixture_path(input);
            assert_eq!(
                candidate(input.to_str().unwrap(), &current, None, None),
                native_fixture_path(expected)
            );
        }
    }

    #[test]
    fn windows_tilde_selection_matches_home_and_named_user_rules() {
        let current = native_fixture_path(r"C:\work");
        let home = native_fixture_path(r"C:\Users\owner");
        let user = Some(OsStr::new("owner"));
        for (input, expected) in [
            ("~", r"C:\Users\owner"),
            (r"~\input", r"C:\Users\owner\input"),
            ("~//input", r"C:\Users\owner\input"),
            (r"~owner\input", r"C:\Users\owner\input"),
            (r"~other\input", r"C:\Users\other\input"),
        ] {
            let input = native_fixture_path(input);
            assert_eq!(
                candidate(input.to_str().unwrap(), &current, Some(&home), user),
                native_fixture_path(expected)
            );
        }
        assert_eq!(
            candidate(
                native_fixture_path(r"~\input").to_str().unwrap(),
                &current,
                None,
                user
            ),
            current.join(native_fixture_path(r"~\input"))
        );
        for unmatched in [
            native_fixture_path(r"C:\Custom\profile"),
            native_fixture_path("C:\\Users\\owner\\"),
        ] {
            assert_eq!(
                candidate(
                    native_fixture_path(r"~other\input").to_str().unwrap(),
                    &current,
                    Some(&unmatched),
                    user
                ),
                current.join(native_fixture_path(r"~other\input"))
            );
        }
        assert_eq!(
            candidate(
                native_fixture_path(r"~owner\input").to_str().unwrap(),
                &current,
                Some(&home),
                None
            ),
            current.join(native_fixture_path(r"~owner\input"))
        );
    }
}
