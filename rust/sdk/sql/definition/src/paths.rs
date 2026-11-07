// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    DefinitionError, DefinitionErrorKind as Kind, DefinitionLimitKind as Limit, DefinitionLimits,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct Paths {
    files: BTreeMap<String, String>,
    prefixes: BTreeMap<String, String>,
    directories: BTreeSet<String>,
}

impl Paths {
    pub(crate) fn insert(
        &mut self,
        path: &str,
        limits: DefinitionLimits,
        index: usize,
    ) -> Result<(), DefinitionError> {
        validate(path, limits, index)?;
        let key = path.to_lowercase();
        if let Some(previous) = self.files.get(&key) {
            return Err(DefinitionError::new(
                if previous == path {
                    Kind::DuplicatePath
                } else {
                    Kind::CaseCollision
                },
                Some(index),
            ));
        }
        if self.directories.contains(&key) {
            return Err(DefinitionError::new(
                Kind::FileDirectoryCollision,
                Some(index),
            ));
        }
        let mut prefix = String::new();
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let prefix_key = prefix.to_lowercase();
            if self
                .prefixes
                .get(&prefix_key)
                .is_some_and(|previous| previous != &prefix)
            {
                return Err(DefinitionError::new(Kind::CaseCollision, Some(index)));
            }
            if components.peek().is_some() {
                if self.files.contains_key(&prefix_key) {
                    return Err(DefinitionError::new(
                        Kind::FileDirectoryCollision,
                        Some(index),
                    ));
                }
                self.directories.insert(prefix_key.clone());
            }
            self.prefixes.insert(prefix_key, prefix.clone());
        }
        self.files.insert(key, path.to_owned());
        Ok(())
    }
}

fn validate(path: &str, limits: DefinitionLimits, index: usize) -> Result<(), DefinitionError> {
    if path.len() > limits.max_path_bytes.get() {
        return Err(DefinitionError::limit(Limit::PathBytes, Some(index)));
    }
    if path.split('/').count() > limits.max_depth.get() {
        return Err(DefinitionError::limit(Limit::Depth, Some(index)));
    }
    for component in path.split('/') {
        let stem = component
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                    || matches!(suffix, "\u{b9}" | "\u{b2}" | "\u{b3}")
            })
        });
        if matches!(component, "" | "." | "..")
            || reserved
            || component.ends_with(['.', ' '])
            || component
                .chars()
                .any(|ch| ch.is_control() || "\\:*?\"<>|".contains(ch))
        {
            return Err(DefinitionError::new(Kind::UnsafePath, Some(index)));
        }
    }
    Ok(())
}
