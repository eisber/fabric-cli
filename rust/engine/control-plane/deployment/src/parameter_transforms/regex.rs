// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TransformBudget, TransformError};
use fancy_regex::{Expr, Regex, RegexBuilder};
use std::fmt;

/// Bounded Python-source regex subset for configuration filters.
///
/// Unlike find/replace rules, filter patterns permit any capture count. The
/// source syntax translator and explicit unsupported classifications still apply;
/// a raw library compile is never treated as proof of Python equivalence.
#[derive(Clone)]
pub struct SourceRegex {
    regex: Regex,
}

impl fmt::Debug for SourceRegex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRegex")
            .finish_non_exhaustive()
    }
}

impl SourceRegex {
    /// Validate supported Python regex syntax with caller-owned finite bounds.
    ///
    /// # Errors
    /// Preserves invalid source syntax, unsupported advanced features and limits.
    pub fn compile(
        pattern: &str,
        budget: &mut TransformBudget<'_>,
    ) -> Result<Self, TransformError> {
        budget.checkpoint()?;
        if pattern.len() > budget.limits.max_pattern_bytes {
            return Err(TransformError::Limit);
        }
        let mut depth = 0_usize;
        let mut escaped = false;
        let mut in_class = false;
        for character in pattern.chars() {
            budget.checkpoint()?;
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '[' => in_class = true,
                ']' => in_class = false,
                '(' if !in_class => {
                    depth = depth.checked_add(1).ok_or(TransformError::Limit)?;
                    budget.depth(depth)?;
                }
                ')' if !in_class => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let translated = translate(pattern, false)?;
        let tree = Expr::parse_tree(&translated).map_err(|error| compile_error(error, pattern))?;
        validate_tree(&tree.expr, budget, 0)?;
        let regex = RegexBuilder::new(&translated)
            .backtrack_limit(budget.limits.max_backtracks)
            .delegate_size_limit(budget.limits.max_regex_bytes)
            .delegate_dfa_size_limit(budget.limits.max_regex_bytes)
            .build()
            .map_err(|error| compile_error(error, pattern))?;
        budget.checkpoint()?;
        Ok(Self { regex })
    }

    /// Source `re.search` semantics, used by folder exclusions.
    ///
    /// # Errors
    /// Preserves bounded search/cancellation/resource errors.
    pub fn search(
        &self,
        text: &str,
        budget: &mut TransformBudget<'_>,
    ) -> Result<bool, TransformError> {
        budget.text(text)?;
        let result = self
            .regex
            .is_match(text)
            .map_err(|_| TransformError::Limit)?;
        budget.checkpoint()?;
        Ok(result)
    }

    /// Source `re.match` semantics, used by item-name and shortcut exclusions.
    ///
    /// # Errors
    /// Preserves bounded search/cancellation/resource errors.
    pub fn matches_start(
        &self,
        text: &str,
        budget: &mut TransformBudget<'_>,
    ) -> Result<bool, TransformError> {
        budget.text(text)?;
        let result = self
            .regex
            .find(text)
            .map_err(|_| TransformError::Limit)?
            .is_some_and(|matched| matched.start() == 0);
        budget.checkpoint()?;
        Ok(result)
    }
}

/// Compiled source find rule; replacements are always literal, never templates.
#[derive(Clone)]
pub struct FindReplacement {
    pattern: String,
    regex: Option<Regex>,
    capture_only: bool,
    ignore_case: bool,
}

impl fmt::Debug for FindReplacement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FindReplacement")
            .field("capture_only", &self.capture_only)
            .field("ignore_case", &self.ignore_case)
            .finish_non_exhaustive()
    }
}

impl FindReplacement {
    pub(super) fn has_matches(
        &self,
        content: &str,
        filter_match: bool,
        budget: &mut TransformBudget<'_>,
    ) -> Result<bool, TransformError> {
        budget.text(content)?;
        if !filter_match {
            return Ok(false);
        }
        if !self.capture_only {
            return Ok(if self.ignore_case {
                content
                    .to_lowercase()
                    .contains(&self.pattern.to_lowercase())
            } else {
                content.contains(&self.pattern)
            });
        }
        let captures = self
            .regex
            .as_ref()
            .expect("compiled regex")
            .captures(content)
            .map_err(|_| TransformError::Limit)?;
        if let Some(captures) = captures {
            if captures
                .get(1)
                .is_none_or(|value| value.as_str().is_empty())
            {
                return Err(TransformError::Invalid(format!(
                    "Regex pattern '{}' captured an empty value.",
                    self.pattern
                )));
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Validate/compile even if a later file filter excludes this rule.
    ///
    /// # Errors
    /// Rejects invalid patterns or anything except exactly one regex capture;
    /// syntax/allocation/backtracking/time bounds and unsupported syntax are typed.
    pub fn compile(
        pattern: &str,
        is_regex: bool,
        ignore_case: bool,
        budget: &mut TransformBudget<'_>,
    ) -> Result<Self, TransformError> {
        budget.checkpoint()?;
        if pattern.len() > budget.limits.max_pattern_bytes {
            return Err(TransformError::Limit);
        }
        if pattern.is_empty() {
            return Err(TransformError::Invalid("Empty find_value".into()));
        }
        let mut depth = 0_usize;
        let mut escaped = false;
        let mut in_class = false;
        for character in pattern.chars() {
            budget.checkpoint()?;
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '[' => in_class = true,
                ']' => in_class = false,
                '(' if !in_class => {
                    depth = depth.checked_add(1).ok_or(TransformError::Limit)?;
                    budget.depth(depth)?;
                }
                ')' if !in_class => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let compiled = if is_regex || ignore_case {
            let source = if is_regex {
                translate(pattern, ignore_case)?
            } else {
                translate(&fancy_regex::escape(pattern), ignore_case)?
            };
            let tree = Expr::parse_tree(&source).map_err(|error| compile_error(error, pattern))?;
            validate_tree(&tree.expr, budget, 0)?;
            let regex = RegexBuilder::new(&source)
                .case_insensitive(ignore_case)
                .backtrack_limit(budget.limits.max_backtracks)
                .delegate_size_limit(budget.limits.max_regex_bytes)
                .delegate_dfa_size_limit(budget.limits.max_regex_bytes)
                .build()
                .map_err(|error| compile_error(error, pattern))?;
            if is_regex && regex.captures_len() != 2 {
                return Err(TransformError::Invalid(format!(
                    "Regex pattern '{pattern}' must contain exactly one capturing group."
                )));
            }
            Some(regex)
        } else {
            None
        };
        budget.checkpoint()?;
        Ok(Self {
            pattern: pattern.into(),
            regex: compiled,
            capture_only: is_regex,
            ignore_case,
        })
    }

    /// Apply capture-1 slicing or a case-sensitive/case-insensitive literal rule.
    ///
    /// Only the first regex capture is validated for nonemptiness, matching
    /// Python. Later unmatched captures retain Python's negative slicing offsets.
    /// Empty replacements preserve source no-op bytes after capture validation.
    ///
    /// # Errors
    /// Retains invalid/empty capture and finite resource/cancellation failures.
    pub fn apply(
        &self,
        content: &str,
        replacement: &str,
        filter_match: bool,
        budget: &mut TransformBudget<'_>,
    ) -> Result<String, TransformError> {
        budget.text(content)?;
        budget.text(replacement)?;
        if !filter_match {
            return Ok(content.into());
        }
        if !self.capture_only
            && self.ignore_case
            && !content
                .to_lowercase()
                .contains(&self.pattern.to_lowercase())
        {
            return Ok(content.into());
        }
        let Some(regex) = &self.regex else {
            if replacement.is_empty() {
                return Ok(content.into());
            }
            let mut output = String::new();
            let mut previous = 0;
            for (index, _) in content.match_indices(&self.pattern) {
                budget.matched()?;
                budget.push(&mut output, &content[previous..index])?;
                budget.push(&mut output, replacement)?;
                previous = index + self.pattern.len();
            }
            budget.push(&mut output, &content[previous..])?;
            return Ok(output);
        };
        let mut output = String::new();
        let mut previous = 0;
        let mut first = true;
        for captures in regex.captures_iter(content) {
            budget.checkpoint()?;
            let captures = captures.map_err(|_| TransformError::Limit)?;
            budget.matched()?;
            let whole = captures.get(0).expect("regex capture0 always exists");
            let group = captures.get(1);
            if self.capture_only && first && group.is_none_or(|value| value.as_str().is_empty()) {
                return Err(TransformError::Invalid(format!(
                    "Regex pattern '{}' captured an empty value.",
                    self.pattern
                )));
            }
            first = false;
            if replacement.is_empty() {
                continue;
            }
            budget.push(&mut output, &content[previous..whole.start()])?;
            if self.capture_only {
                let full = whole.as_str();
                let whole_position = content[..whole.start()].chars().count() as i128;
                let (start, end) = group.map_or((-1, -1), |group| {
                    (
                        content[..group.start()].chars().count() as i128,
                        content[..group.end()].chars().count() as i128,
                    )
                });
                let prefix = slice_index(full, start - whole_position);
                let suffix = slice_index(full, end - whole_position);
                budget.push(&mut output, &full[..prefix])?;
                budget.push(&mut output, replacement)?;
                budget.push(&mut output, &full[suffix..])?;
            } else {
                budget.push(&mut output, replacement)?;
            }
            previous = whole.end();
        }
        if replacement.is_empty() {
            return Ok(content.into());
        }
        budget.push(&mut output, &content[previous..])?;
        Ok(output)
    }
}

fn slice_index(text: &str, index: i128) -> usize {
    let length = text.chars().count() as i128;
    let index = if index < 0 {
        (length + index).max(0)
    } else {
        index.min(length)
    };
    text.char_indices()
        .nth(index as usize)
        .map_or(text.len(), |(index, _)| index)
}

fn compile_error(error: fancy_regex::Error, pattern: &str) -> TransformError {
    match error {
        fancy_regex::Error::ParseError(_, fancy_regex::ParseError::RecursionExceeded) => {
            TransformError::Limit
        }
        fancy_regex::Error::CompileError(fancy_regex::CompileError::InnerError(error))
            if error.size_limit().is_some() =>
        {
            TransformError::Limit
        }
        fancy_regex::Error::CompileError(fancy_regex::CompileError::FeatureNotYetSupported(_)) => {
            TransformError::Unsupported("regex engine compilation feature")
        }
        _ => TransformError::Invalid(format!("Invalid regex '{pattern}'")),
    }
}

fn validate_tree(
    expr: &Expr,
    budget: &mut TransformBudget<'_>,
    depth: usize,
) -> Result<(), TransformError> {
    budget.depth(depth)?;
    match expr {
        Expr::Concat(children) | Expr::Alt(children) => {
            for child in children {
                validate_tree(child, budget, depth + 1)?;
            }
        }
        Expr::Group(child)
        | Expr::LookAround(child, _)
        | Expr::AtomicGroup(child)
        | Expr::Repeat { child, .. } => validate_tree(child, budget, depth + 1)?,
        Expr::Empty
        | Expr::Any { .. }
        | Expr::Assertion(_)
        | Expr::Literal { .. }
        | Expr::Delegate { .. }
        | Expr::Backref { .. } => {}
        _ => {
            return Err(TransformError::Unsupported(
                "advanced regex control/subroutine syntax",
            ));
        }
    }
    Ok(())
}

fn translate(pattern: &str, ignore_case: bool) -> Result<String, TransformError> {
    for (index, _) in pattern.match_indices("(?") {
        let suffix = &pattern[index..];
        if ![
            "(?:", "(?=", "(?!", "(?<=", "(?<!", "(?P<", "(?P=", "(?>", "(?#",
        ]
        .iter()
        .any(|prefix| suffix.starts_with(prefix))
        {
            return Err(TransformError::Unsupported(
                "inline flags or non-Python regex group syntax",
            ));
        }
    }
    let mut output = String::new();
    let mut characters = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(character) = characters.next() {
        if character == '\\' {
            let escaped = characters
                .next()
                .ok_or_else(|| TransformError::Invalid(format!("Invalid regex '{pattern}'")))?;
            if matches!(escaped, 'u' | 'x') && characters.peek() == Some(&'{') {
                return Err(TransformError::Unsupported(
                    "Rust-style braced regex escape",
                ));
            }
            let translated = match escaped {
                'w' => {
                    if in_class {
                        r"\p{L}\p{N}_"
                    } else {
                        r"[\p{L}\p{N}_]"
                    }
                }
                'W' if !in_class => r"[^\p{L}\p{N}_]",
                'd' => r"\p{Nd}",
                'D' => r"\P{Nd}",
                's' => {
                    if in_class {
                        r"\t\n\r\x0b\x0c\x1c-\x1f\u{0085}\p{Z}"
                    } else {
                        r"[\t\n\r\x0b\x0c\x1c-\x1f\u{0085}\p{Z}]"
                    }
                }
                'S' if !in_class => r"[^\t\n\r\x0b\x0c\x1c-\x1f\u{0085}\p{Z}]",
                'Z' => r"\z",
                'p' | 'P' | 'z' | 'K' | 'G' | 'g' | 'k' | 'h' | 'H' | 'e' | 'R' | 'W' | 'S'
                | 'N' | 'b' | 'B' => {
                    return Err(TransformError::Unsupported(
                        "unproven regex escape/class semantics",
                    ));
                }
                _ => {
                    output.push('\\');
                    output.push(escaped);
                    continue;
                }
            };
            output.push_str(translated);
        } else if character == '('
            && (characters.clone().take(3).collect::<String>() == "?P<"
                || characters.clone().take(3).collect::<String>() == "?P=")
        {
            let delimiter = if characters.clone().nth(2) == Some('<') {
                '>'
            } else {
                ')'
            };
            output.push(character);
            for character in characters.by_ref() {
                output.push(character);
                if character == delimiter {
                    break;
                }
            }
        } else if in_class
            && ((matches!(character, '&' | '~' | '|' | '-')
                && characters.peek() == Some(&character))
                || character == '[')
        {
            return Err(TransformError::Unsupported(
                "regex class set operations or nested classes",
            ));
        } else if character == '[' {
            in_class = true;
            output.push(character);
        } else if character == ']' {
            in_class = false;
            output.push(character);
        } else if character == '$' && !in_class {
            output.push_str(r"(?=\n?\z)");
        } else if ignore_case && matches!(character, 'i' | 'I' | '\u{0130}' | '\u{0131}') {
            output.push_str(if in_class {
                "iI\u{0130}\u{0131}"
            } else {
                "[iI\u{0130}\u{0131}]"
            });
        } else {
            output.push(character);
        }
    }
    if pattern.contains("(?")
        && (pattern.contains("(?i")
            || pattern.contains("(?m")
            || pattern.contains("(?s")
            || pattern.contains("(?a")
            || pattern.contains("(?x")
            || pattern.contains("(?u")
            || pattern.contains("(?-"))
    {
        return Err(TransformError::Unsupported("inline regex flags"));
    }
    if ignore_case && pattern.contains('-') && pattern.contains('[') {
        return Err(TransformError::Unsupported(
            "ignore-case regex character ranges",
        ));
    }
    Ok(output)
}
