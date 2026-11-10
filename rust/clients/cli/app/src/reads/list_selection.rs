// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    errors,
    parser::{ReadCommand, ReadKind},
};
use fabric_cli_output::CommandError;

/// An exact listing parent and optional local child-name prefix.
pub(crate) struct ListSelection {
    pub(crate) path: String,
    prefix: Option<String>,
}

impl ListSelection {
    pub(crate) fn new(command: &ReadCommand) -> Result<Self, CommandError> {
        let path = command.path.join(" ");
        let path = path.trim_matches(['\'', '"']).to_owned();
        if !path.contains('*') {
            return Ok(Self { path, prefix: None });
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use crate::parser::{Invocation, parse_args};

            #[test]
            fn exact_list_and_get_paths_keep_literal_punctuation() {
                for verb in ["ls", "get"] {
                    for path in ["/Demo?.Workspace", "/Demo[.Workspace", "/Demo].Workspace"] {
                        let Invocation::Read(command) = parse_args(["fab-rs", verb, path]).unwrap()
                        else {
                            panic!("read invocation required");
                        };
                        let selection = ListSelection::new(&command).unwrap();
                        assert_eq!(selection.path, path);
                        assert!(selection.prefix.is_none());
                    }
                }
            }
        }
        let invalid = || {
            errors::error(
                "InvalidPath",
                "Only ls/dir supports one trailing '*' after a nonempty prefix in the final path segment",
            )
        };
        if command.kind != ReadKind::List
            || path.contains(['?', '[', ']'])
            || path.matches('*').count() != 1
        {
            return Err(invalid());
        }
        // Match the resolver's escaped-slash boundary without interpreting the parent.
        let mut boundary = None;
        let mut previous = None;
        for (index, character) in path.char_indices() {
            if character == '/' && previous != Some('\\') {
                boundary = Some(index);
            }
            previous = Some(character);
        }
        let leaf = path[boundary.map_or(0, |index| index + 1)..].replace('\\', "");
        let leaf = leaf.trim_matches(|character: char| {
            character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
        });
        let prefix = leaf
            .strip_suffix('*')
            .filter(|prefix| !prefix.is_empty() && !prefix.starts_with("..."))
            .ok_or_else(invalid)?
            .to_owned();
        let parent = match boundary {
            None => String::new(),
            Some(0) => "/".to_owned(),
            Some(index) => path[..index].to_owned(),
        };
        Ok(Self {
            path: parent,
            prefix: Some(prefix),
        })
    }

    pub(crate) fn matches(&self, name: &str) -> bool {
        let Some(prefix) = self.prefix.as_deref() else {
            return true;
        };
        if name.starts_with(prefix) {
            return true;
        }
        // Listed names already carry canonical known suffixes; only that suffix
        // is case-insensitive, never the resource's display name.
        match (prefix.rsplit_once('.'), name.rsplit_once('.')) {
            (Some((prefix_name, prefix_type)), Some((name, kind))) => {
                prefix_name == name && prefix_type.to_lowercase() == kind.to_lowercase()
            }
            _ => false,
        }
    }
}
