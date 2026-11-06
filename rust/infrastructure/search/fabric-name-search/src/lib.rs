// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure, bounded typo suggestions over caller-supplied names.
//!
//! Suggestions are advisory data, never target selection or authorization. This
//! crate performs no I/O, candidate discovery, authentication, or mutations.
//! Returned names retain their original spelling and may contain untrusted text;
//! consumers must escape them appropriately before presentation.
//!
//! See the crate README for scoring, Unicode semantics, and resource budgets.

#![deny(unnameable_test_items)]

use std::collections::BTreeSet;
use std::fmt;

/// Maximum Unicode scalar count of the query and of each candidate.
pub const MAX_NAME_CHARS: usize = 512;

/// Maximum candidate count, including duplicates and exact matches.
pub const MAX_CANDIDATES: usize = 4096;

/// Maximum cumulative UTF-8 bytes of the query and all candidates.
///
/// Duplicates and exact matches count in full, before deduplication.
pub const MAX_TOTAL_BYTES: usize = 1_048_576;

/// Validated-at-search options for bounded advisory results.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchOptions {
    /// Maximum results to return, in the inclusive range `1..=20`.
    pub max_results: usize,
    /// Inclusive minimum score, finite and in the range `0.0..=1.0`.
    pub min_score: f64,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            max_results: 5,
            min_score: 0.6,
        }
    }
}

/// An advisory name and its deterministic similarity score.
///
/// The borrowed name is the candidate's byte-exact original text, not a
/// normalized spelling. Debug output redacts it; explicitly presenting `name`
/// still requires the caller's appropriate escaping and disclosure policy.
#[derive(Clone, Copy, PartialEq)]
pub struct NameSuggestion<'a> {
    /// Original candidate spelling, potentially containing untrusted text.
    pub name: &'a str,
    /// Finite similarity in the inclusive range `0.0..=1.0`, not a probability.
    pub score: f64,
}

impl fmt::Debug for NameSuggestion<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NameSuggestion")
            .field("name", &"[REDACTED]")
            .field("score", &self.score)
            .finish()
    }
}

/// A bounded validation failure with no query or candidate fragments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchError {
    /// The requested result limit is not in `1..=20`.
    InvalidMaxResults,
    /// The requested score is nonfinite or outside `0.0..=1.0`.
    InvalidMinScore,
    /// The query exceeds [`MAX_NAME_CHARS`] Unicode scalars.
    QueryTooLong,
    /// A candidate exceeds [`MAX_NAME_CHARS`] Unicode scalars.
    CandidateTooLong,
    /// Candidate enumeration exceeds [`MAX_CANDIDATES`].
    TooManyCandidates,
    /// Cumulative query and candidate UTF-8 bytes exceed [`MAX_TOTAL_BYTES`].
    InputTooLarge,
}

impl fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidMaxResults => "Name-search result limit must be between 1 and 20.",
            Self::InvalidMinScore => {
                "Name-search minimum score must be finite and between 0 and 1."
            }
            Self::QueryTooLong => "Name-search query exceeds the character limit.",
            Self::CandidateTooLong => "Name-search candidate exceeds the character limit.",
            Self::TooManyCandidates => "Name-search candidate count exceeds the limit.",
            Self::InputTooLarge => "Name-search input exceeds the cumulative byte limit.",
        })
    }
}

impl std::error::Error for SearchError {}

/// Rank typo-like names from a caller-owned candidate collection.
///
/// Options and the complete bounded input are validated before ranking, including
/// for empty/whitespace queries. A limit violation returns an explicit error,
/// never partial or prefix results. Enumeration stops at the first violation.
///
/// Byte-exact matches are excluded, byte-exact duplicates are collapsed, and
/// ties use ascending original name order. Case-only alternatives can score
/// `1.0`; candidates are never rewritten. Lower thresholds do not disable the
/// conservative edit budget or the single-scalar guard.
///
/// # Errors
///
/// Returns [`SearchError`] for invalid options or an input budget violation.
/// Error diagnostics never contain caller-supplied text.
///
/// # Examples
///
/// ```
/// use fabric_name_search::{SearchOptions, suggest_names};
///
/// let names = ["production_endpoint", "archive_bucket"];
/// let hints = suggest_names("production_endpont", names, SearchOptions::default())?;
/// assert_eq!(hints[0].name, "production_endpoint");
/// // Display a hint if appropriate; do not automatically select or retry it.
/// # Ok::<(), fabric_name_search::SearchError>(())
/// ```
pub fn suggest_names<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    options: SearchOptions,
) -> Result<Vec<NameSuggestion<'a>>, SearchError> {
    validate_options(options)?;
    if exceeds_character_limit(query) {
        return Err(SearchError::QueryTooLong);
    }

    let mut total_bytes = query.len();
    let mut unique = BTreeSet::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        if index >= MAX_CANDIDATES {
            return Err(SearchError::TooManyCandidates);
        }
        total_bytes = total_bytes
            .checked_add(candidate.len())
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or(SearchError::InputTooLarge)?;
        if exceeds_character_limit(candidate) {
            return Err(SearchError::CandidateTooLong);
        }
        unique.insert(candidate);
    }

    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let query_chars = lowercase_chars(query);
    let query_tokens = identifier_tokens(query);
    let query_is_single_scalar = query.chars().count() == 1;
    let mut suggestions = Vec::new();
    for candidate in unique {
        if candidate == query || candidate.trim().is_empty() {
            continue;
        }
        if let Some(score) = similarity(
            &query_chars,
            &query_tokens,
            query_is_single_scalar,
            candidate,
        ) && score >= options.min_score
        {
            suggestions.push(NameSuggestion {
                name: candidate,
                score,
            });
        }
    }
    suggestions.sort_unstable_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.name.cmp(right.name))
    });
    suggestions.truncate(options.max_results);
    Ok(suggestions)
}

fn validate_options(options: SearchOptions) -> Result<(), SearchError> {
    if !(1..=20).contains(&options.max_results) {
        return Err(SearchError::InvalidMaxResults);
    }
    if !options.min_score.is_finite() || !(0.0..=1.0).contains(&options.min_score) {
        return Err(SearchError::InvalidMinScore);
    }
    Ok(())
}

fn exceeds_character_limit(name: &str) -> bool {
    name.chars().take(MAX_NAME_CHARS + 1).count() > MAX_NAME_CHARS
}

fn lowercase_chars(name: &str) -> Vec<char> {
    name.chars().flat_map(char::to_lowercase).collect()
}

fn similarity(
    query: &[char],
    query_tokens: &[Vec<char>],
    query_is_single_scalar: bool,
    candidate: &str,
) -> Option<f64> {
    let candidate_chars = lowercase_chars(candidate);
    if query == candidate_chars.as_slice() {
        return Some(1.0);
    }
    if query_is_single_scalar || candidate.chars().count() <= 1 {
        return None;
    }

    let longest = query.len().max(candidate_chars.len());
    let budget = (longest / 3).max(1);
    let distance = osa_distance(query, &candidate_chars, budget)?;
    let whole_score = 1.0 - distance as f64 / longest as f64;
    let candidate_tokens = identifier_tokens(candidate);
    let lexical_score = token_similarity(query_tokens, &candidate_tokens, whole_score);
    let score = whole_score.min(lexical_score).clamp(0.0, 1.0);
    (score > 0.0).then_some(score)
}

fn identifier_tokens(name: &str) -> Vec<Vec<char>> {
    let chars: Vec<char> = name.chars().collect();
    let mut tokens = Vec::new();
    let mut token = Vec::new();
    let mut previous_letter: Option<char> = None;
    for (index, &character) in chars.iter().enumerate() {
        let separator = character.is_whitespace()
            || character.is_control()
            || matches!(character, '_' | '-' | '.' | '/' | '\\' | ':');
        let camel_boundary = character.is_uppercase()
            && previous_letter.is_some_and(|previous| {
                previous.is_lowercase()
                    || (previous.is_uppercase()
                        && chars.get(index + 1).is_some_and(|next| next.is_lowercase()))
            });
        if (separator || camel_boundary) && !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
        if !separator {
            token.extend(character.to_lowercase());
            if character.is_alphanumeric() {
                previous_letter = Some(character);
            }
        } else {
            previous_letter = None;
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn token_similarity(left: &[Vec<char>], right: &[Vec<char>], whole_score: f64) -> f64 {
    if left.iter().flatten().eq(right.iter().flatten()) {
        return 1.0;
    }
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    if left.len() == 1 {
        return whole_score;
    }

    // The weakest corresponding token prevents a shared prefix/affix from
    // overwhelming an unrelated distinguishing token.
    left.iter()
        .zip(right)
        .map(|(left, right)| {
            let longest = left.len().max(right.len());
            let distance = osa_distance(left, right, longest).unwrap_or(longest);
            1.0 - distance as f64 / longest as f64
        })
        .fold(1.0, f64::min)
}

/// Restricted Damerau-Levenshtein (optimal string alignment) distance.
///
/// Three rolling rows retain adjacent-transposition history. A diagonal band
/// bounds work to the requested distance; cells outside it use a sentinel.
fn osa_distance(left: &[char], right: &[char], budget: usize) -> Option<usize> {
    let (left, right) = if left.len() < right.len() {
        (right, left)
    } else {
        (left, right)
    };
    if left.len() - right.len() > budget {
        return None;
    }
    let sentinel = budget + 1;
    let mut older = vec![sentinel; right.len() + 1];
    let mut previous: Vec<usize> = (0..=right.len()).map(|value| value.min(sentinel)).collect();
    let mut current = vec![sentinel; right.len() + 1];
    for row in 1..=left.len() {
        current.fill(sentinel);
        current[0] = row.min(sentinel);
        let start = row.saturating_sub(budget).max(1);
        let end = (row + budget).min(right.len());
        for column in start..=end {
            let substitution = usize::from(left[row - 1] != right[column - 1]);
            let mut distance = (previous[column] + 1)
                .min(current[column - 1] + 1)
                .min(previous[column - 1] + substitution);
            if row > 1
                && column > 1
                && left[row - 1] == right[column - 2]
                && left[row - 2] == right[column - 1]
            {
                distance = distance.min(older[column - 2] + 1);
            }
            current[column] = distance.min(sentinel);
        }
        std::mem::swap(&mut older, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    (previous[right.len()] <= budget).then_some(previous[right.len()])
}

#[cfg(test)]
mod tests;
