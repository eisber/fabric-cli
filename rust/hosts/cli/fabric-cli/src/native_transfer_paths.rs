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

    #[test]
    fn explicit_candidates_preserve_absolute_roots_and_defer_path_validation() {
        let current = Path::new(r"C:\work");
        for (input, expected) in [
            ("input", r"C:\work\input"),
            (r"D:\outside\input", r"D:\outside\input"),
            (r"..\input", r"C:\work\..\input"),
            (r"\input", r"C:\input"),
        ] {
            assert_eq!(candidate(input, current, None, None), Path::new(expected));
        }
    }

    #[test]
    fn windows_tilde_selection_matches_home_and_named_user_rules() {
        let current = Path::new(r"C:\work");
        let home = Path::new(r"C:\Users\owner");
        let user = Some(OsStr::new("owner"));
        for (input, expected) in [
            ("~", r"C:\Users\owner"),
            (r"~\input", r"C:\Users\owner\input"),
            ("~//input", r"C:\Users\owner\input"),
            (r"~owner\input", r"C:\Users\owner\input"),
            (r"~other\input", r"C:\Users\other\input"),
        ] {
            assert_eq!(
                candidate(input, current, Some(home), user),
                Path::new(expected)
            );
        }
        assert_eq!(
            candidate(r"~\input", current, None, user),
            current.join(r"~\input")
        );
        for unmatched in [
            Path::new(r"C:\Custom\profile"),
            Path::new("C:\\Users\\owner\\"),
        ] {
            assert_eq!(
                candidate(r"~other\input", current, Some(unmatched), user),
                current.join(r"~other\input")
            );
        }
        assert_eq!(
            candidate(r"~owner\input", current, Some(home), None),
            current.join(r"~owner\input")
        );
    }
}
