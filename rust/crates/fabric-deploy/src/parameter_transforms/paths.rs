// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TransformBudget, TransformError};

/// Caller-selected host path semantics; no ambient filesystem is consulted.
#[derive(Clone, Copy, Debug)]
pub struct PathFilterOptions {
    /// Windows regular paths recognize backslashes and drive-root prefixes.
    pub windows_paths: bool,
    /// Pathlib glob/equality casing for the source host.
    pub case_sensitive: bool,
}

impl Default for PathFilterOptions {
    fn default() -> Self {
        Self {
            windows_paths: cfg!(windows),
            case_sensitive: !cfg!(windows),
        }
    }
}

/// Concrete matches over the caller's validated regular-file inventory.
#[derive(Clone, Debug)]
pub struct PathFilterResolution {
    /// Original inventory spelling, in inventory order; duplicates are removed.
    pub paths: Vec<String>,
    /// Filters rejected by source syntax/confinement or matching no regular files.
    pub skipped: Vec<String>,
}

/// Resolve source regular/glob path filters without reading the filesystem.
///
/// `repository_files` contains caller-validated, canonical repository-relative
/// regular-file paths. The caller owns symlink resolution and stable-file checks.
/// Results use this same relative domain for [`super::ParameterItem::file_path`].
/// Glob braces remain literal, as with pathlib, rather than shell expansion.
///
/// # Errors
/// Rejects invalid inventory, unsupported drive-relative paths and exhausted
/// syntax/visit/allocation/cancellation bounds. Source-rejected filters are
/// returned as skip outcomes, never treated as unrestricted matches.
pub fn resolve_parameter_paths(
    repository_root: &str,
    filters: &[String],
    repository_files: &[String],
    options: PathFilterOptions,
    budget: &mut TransformBudget<'_>,
) -> Result<PathFilterResolution, TransformError> {
    budget.text(repository_root)?;
    let root = repository_root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    let fold = |value: &str| {
        if options.case_sensitive {
            value.to_owned()
        } else {
            value.to_lowercase()
        }
    };
    let mut inventory = Vec::new();
    let mut inventory_bytes = 0_usize;
    for file in repository_files {
        budget.text(file)?;
        inventory_bytes = inventory_bytes
            .checked_add(file.len())
            .filter(|bytes| *bytes <= budget.limits.max_bytes)
            .ok_or(TransformError::Limit)?;
        let normalized = if options.windows_paths {
            file.replace('\\', "/")
        } else {
            file.clone()
        };
        if normalized.starts_with('/') || normalized.contains(':') {
            return Err(TransformError::Invalid(
                "Inventory must use repository-relative paths".into(),
            ));
        }
        let relative = normalize(&normalized).ok_or_else(|| {
            TransformError::Invalid("Invalid repository-relative file inventory".into())
        })?;
        if relative.starts_with('/') || relative.contains(':') {
            return Err(TransformError::Invalid(
                "Inventory must use repository-relative paths".into(),
            ));
        }
        inventory.push((file, relative));
    }
    let mut output = PathFilterResolution {
        paths: Vec::new(),
        skipped: Vec::new(),
    };
    for filter in filters {
        budget.text(filter)?;
        if filter.len() > budget.limits.max_pattern_bytes {
            return Err(TransformError::Limit);
        }
        let glob = filter.contains(['*', '?', '[']);
        let mut pattern = if glob || options.windows_paths {
            filter.replace('\\', "/")
        } else {
            filter.clone()
        };
        if !glob {
            pattern = pattern.trim_start_matches(['/', '\\']).into();
        }
        if glob && !valid_glob(&pattern) {
            output.skipped.push(filter.clone());
            continue;
        }
        let drive = options.windows_paths && pattern.as_bytes().get(1) == Some(&b':');
        if drive && pattern.as_bytes().get(2) != Some(&b'/') {
            return Err(TransformError::Unsupported(
                "drive-relative filters require caller path normalization",
            ));
        }
        if pattern.starts_with('/') || drive {
            let prefix = format!("{}/", fold(root));
            if !fold(&pattern).starts_with(&prefix) {
                output.skipped.push(filter.clone());
                continue;
            }
            pattern = pattern
                .split('/')
                .skip(root.split('/').count())
                .collect::<Vec<_>>()
                .join("/");
        }
        let Some(pattern) = normalize(&pattern) else {
            output.skipped.push(filter.clone());
            continue;
        };
        let parts: Vec<_> = pattern.split('/').collect();
        if glob
            && parts
                .iter()
                .any(|part| part.contains("**") && *part != "**")
        {
            output.skipped.push(filter.clone());
            continue;
        }
        let mut matched = false;
        for (original, file) in &inventory {
            budget.checkpoint()?;
            let selected = if glob {
                path_matches(
                    &parts,
                    &file.split('/').collect::<Vec<_>>(),
                    options.case_sensitive,
                    budget,
                )?
            } else {
                fold(&pattern) == fold(file)
            };
            if selected {
                matched = true;
                if !output.paths.contains(original) {
                    output.paths.push(original.to_string());
                }
            }
        }
        if !matched {
            output.skipped.push(filter.clone());
        }
    }
    Ok(output)
}

fn normalize(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn valid_glob(pattern: &str) -> bool {
    if pattern.trim().is_empty()
        || pattern.starts_with("/**/")
        || [
            "/**/*/", "**/**", "//", "\\\\", "**/**/", "../", "..%2f", "..%5c",
        ]
        .iter()
        .any(|part| pattern.to_lowercase().contains(part))
        || (pattern.contains("**") && !pattern.contains("**/") && !pattern.contains("/**"))
    {
        return false;
    }
    let mut stack = Vec::new();
    for character in pattern.chars() {
        match character {
            '[' | '{' => stack.push(character),
            ']' if stack.pop() != Some('[') => return false,
            '}' if stack.pop() != Some('{') => return false,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return false;
    }
    for (open, close) in [('[', ']'), ('{', '}')] {
        let mut remainder = pattern;
        while let Some(start) = remainder.find(open) {
            remainder = &remainder[start + 1..];
            let Some(end) = remainder.find(close) else {
                return false;
            };
            let section = &remainder[..end];
            if section.is_empty()
                || (open == '[' && (section.starts_with([']', '-']) || section.contains("--")))
                || (open == '{'
                    && (!section.contains(',')
                        || section.starts_with(',')
                        || section.ends_with(',')
                        || section.contains(",,")))
            {
                return false;
            }
            remainder = &remainder[end + 1..];
        }
    }
    true
}

fn path_matches(
    pattern: &[&str],
    path: &[&str],
    case_sensitive: bool,
    budget: &mut TransformBudget<'_>,
) -> Result<bool, TransformError> {
    let mut previous = vec![false; path.len() + 1];
    previous[0] = true;
    for part in pattern {
        let mut next = vec![false; path.len() + 1];
        if *part == "**" {
            next[0] = previous[0];
        }
        for (index, value) in path.iter().enumerate() {
            budget.checkpoint()?;
            next[index + 1] = if *part == "**" {
                previous[index + 1] || next[index]
            } else {
                previous[index] && segment_matches(part, value, case_sensitive, budget)?
            };
        }
        previous = next;
    }
    Ok(previous[path.len()])
}

fn segment_matches(
    pattern: &str,
    text: &str,
    case_sensitive: bool,
    budget: &mut TransformBudget<'_>,
) -> Result<bool, TransformError> {
    let pattern: Vec<_> = pattern.chars().collect();
    let text: Vec<_> = text.chars().collect();
    if pattern
        .len()
        .checked_mul(text.len())
        .is_none_or(|work| work > budget.limits.max_visits)
    {
        return Err(TransformError::Limit);
    }
    let mut previous = vec![false; text.len() + 1];
    previous[0] = true;
    let mut index = 0;
    while index < pattern.len() {
        budget.checkpoint()?;
        let character = pattern[index];
        let end = if character == '[' {
            pattern[index + 1..]
                .iter()
                .position(|character| *character == ']')
                .map(|end| index + 1 + end)
                .filter(|end| !(*end == index + 2 && pattern[index + 1] == '!'))
        } else {
            None
        };
        let mut next = vec![false; text.len() + 1];
        if character == '*' {
            next[0] = previous[0];
        }
        for (position, value) in text.iter().enumerate() {
            budget.checkpoint()?;
            next[position + 1] = if character == '*' {
                previous[position + 1] || next[position]
            } else {
                let selected = if let Some(end) = end {
                    class_matches(&pattern[index + 1..end], *value, case_sensitive, budget)?
                } else {
                    character == '?' || equal_character(character, *value, case_sensitive)
                };
                previous[position] && selected
            };
        }
        previous = next;
        index = end.map_or(index + 1, |end| end + 1);
    }
    Ok(previous[text.len()])
}

fn class_matches(
    class: &[char],
    value: char,
    case_sensitive: bool,
    budget: &mut TransformBudget<'_>,
) -> Result<bool, TransformError> {
    let negated = class.first() == Some(&'!');
    let mut index = usize::from(negated);
    let mut matched = false;
    while index < class.len() {
        budget.checkpoint()?;
        if class.get(index + 1) == Some(&'-') && index + 2 < class.len() {
            let mut candidates = vec![value];
            if !case_sensitive {
                let lower: Vec<_> = value.to_lowercase().collect();
                if lower.len() == 1 {
                    candidates.push(lower[0]);
                }
                let upper: Vec<_> = value.to_uppercase().collect();
                if upper.len() == 1 {
                    candidates.push(upper[0]);
                    let lower: Vec<_> = upper[0].to_lowercase().collect();
                    if lower.len() == 1 {
                        candidates.push(lower[0]);
                    }
                }
                if matches!(value, '\u{0130}' | '\u{0131}') {
                    candidates.extend(['i', 'I']);
                }
            }
            matched |= candidates
                .into_iter()
                .any(|value| class[index] <= value && value <= class[index + 2]);
            index += 3;
        } else {
            matched |= equal_character(class[index], value, case_sensitive);
            index += 1;
        }

        fn equal_character(left: char, right: char, case_sensitive: bool) -> bool {
            if left == right {
                return true;
            }
            if case_sensitive {
                return false;
            }
            if matches!(left, 'i' | 'I' | '\u{0130}' | '\u{0131}')
                && matches!(right, 'i' | 'I' | '\u{0130}' | '\u{0131}')
            {
                return true;
            }
            if left.to_lowercase().eq(right.to_lowercase()) {
                return true;
            }
            let left: Vec<_> = left.to_uppercase().collect();
            let right: Vec<_> = right.to_uppercase().collect();
            left.len() == 1 && right.len() == 1 && left == right
        }
    }
    Ok(if negated { !matched } else { matched })
}
