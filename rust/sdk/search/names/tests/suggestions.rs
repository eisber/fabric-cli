// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public-boundary contracts using synthetic names only.

#![deny(unnameable_test_items)]

use fabric_name_search::{
    MAX_CANDIDATES, MAX_NAME_CHARS, MAX_TOTAL_BYTES, NameSuggestion, SearchError, SearchOptions,
    suggest_names,
};

fn options(min_score: f64) -> SearchOptions {
    SearchOptions {
        max_results: 20,
        min_score,
    }
}

fn names<'a>(suggestions: &[NameSuggestion<'a>]) -> Vec<&'a str> {
    suggestions
        .iter()
        .map(|suggestion| suggestion.name)
        .collect()
}

fn only_score(query: &str, candidate: &str) -> f64 {
    let result = suggest_names(query, [candidate], options(0.0)).unwrap();
    assert_eq!(result.len(), 1);
    let score = result[0].score;
    assert!(score.is_finite() && (0.0..=1.0).contains(&score));
    score
}

#[test]
fn defaults_are_pinned() {
    assert_eq!(
        SearchOptions::default(),
        SearchOptions {
            max_results: 5,
            min_score: 0.6,
        }
    );
}

#[test]
fn endpoint_omission_and_adjacent_transposition_have_close_hints() {
    for query in ["production_endpont", "prodcution_endpoint"] {
        let suggestions = suggest_names(
            query,
            [
                "production_account",
                "archive_bucket",
                "production_endpoint",
            ],
            SearchOptions::default(),
        )
        .unwrap();
        assert_eq!(names(&suggestions), ["production_endpoint"]);
        assert!(suggestions[0].score >= 0.85);
    }
}

#[test]
fn whole_identifier_scores_establish_actual_edit_distances() {
    for (query, candidate, expected) in [
        ("abcd", "abdc", 0.75),
        ("abcd", "abxd", 0.75),
        ("abcd", "abc", 0.75),
        ("abcd", "abcde", 0.8),
        ("abcdef", "badcef", 1.0 - 2.0 / 6.0),
        ("é猫🦀", "猫é🦀", 1.0 - 1.0 / 3.0),
    ] {
        assert!((only_score(query, candidate) - expected).abs() < 1e-12);
    }
}

#[test]
fn snake_kebab_and_camel_names_support_typos_without_rewriting() {
    for (query, candidate) in [
        ("production_endpont", "production_endpoint"),
        ("production-endpont", "production-endpoint"),
        ("productionEndpont", "productionEndpoint"),
        ("httpEndpont", "httpEndpoint"),
        ("HTTPEndpont", "HTTPEndpoint"),
        ("production_endpoint", "productionEndpoint"),
        ("production-endpoint", "production_endpoint"),
    ] {
        let result = suggest_names(query, [candidate], SearchOptions::default()).unwrap();
        assert_eq!(names(&result), [candidate]);
    }
}

#[test]
fn weak_distinguishing_tokens_are_not_hidden_by_long_shared_affixes() {
    let prefix = "shared".repeat(60);
    for (query, candidate) in [
        (format!("{prefix}_endpoint"), format!("{prefix}_account")),
        (format!("endpoint_{prefix}"), format!("account_{prefix}")),
        (
            "production_endpoint".to_owned(),
            "production_endpoint_backup".to_owned(),
        ),
    ] {
        assert!(
            suggest_names(&query, [candidate.as_str()], SearchOptions::default())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn unrelated_empty_and_whitespace_inputs_have_no_hints() {
    for query in ["", " ", "\t\r\n", "\u{2003}", "zzzzzzzz"] {
        assert!(
            suggest_names(
                query,
                ["production_endpoint", "archive_bucket", "", "  "],
                SearchOptions::default(),
            )
            .unwrap()
            .is_empty()
        );
    }
    assert!(
        suggest_names("known_name", [], SearchOptions::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn zero_threshold_keeps_conservative_edit_and_single_scalar_guards() {
    assert!(
        suggest_names("aaaa", ["zzzz", "a"], options(0.0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        names(&suggest_names("x", ["y", "xy", "X"], options(0.0)).unwrap()),
        ["X"]
    );
    assert!(
        suggest_names("İ", ["x\u{307}"], options(0.0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(only_score("ab", "ac"), 0.5);
    assert!(
        suggest_names("ab", ["ac"], SearchOptions::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn exact_matches_are_excluded_but_case_only_alternatives_preserve_spelling() {
    let result = suggest_names(
        "Endpoint",
        ["Endpoint", "ENDPOINT", "endpoint", "Endpoint"],
        options(1.0),
    )
    .unwrap();
    assert_eq!(names(&result), ["ENDPOINT", "endpoint"]);
    assert!(result.iter().all(|suggestion| suggestion.score == 1.0));
}

#[test]
fn threshold_is_inclusive_and_one_excludes_nonidentical_normalizations() {
    let score = only_score("abcd", "abdc");
    assert_eq!(
        names(&suggest_names("abcd", ["abdc"], options(score)).unwrap()),
        ["abdc"]
    );
    assert!(
        suggest_names("abcd", ["abdc"], options(score + 1e-10))
            .unwrap()
            .is_empty()
    );
    assert!(
        suggest_names("abcd", ["abdc"], options(1.0))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn results_are_deduplicated_sorted_and_limited_independent_of_enumeration() {
    let candidates = [
        "identifier_006",
        "identifier_001",
        "identifier_003",
        "identifier_002",
        "identifier_005",
        "identifier_004",
        "identifier_001",
        "identifier_000",
    ];
    let forward = suggest_names("identifier_000", candidates, SearchOptions::default()).unwrap();
    let backward = suggest_names(
        "identifier_000",
        candidates.into_iter().rev(),
        SearchOptions::default(),
    )
    .unwrap();
    assert_eq!(forward, backward);
    assert_eq!(
        names(&forward),
        [
            "identifier_001",
            "identifier_002",
            "identifier_003",
            "identifier_004",
            "identifier_005",
        ]
    );
}

#[test]
fn score_order_precedes_lexical_tie_order() {
    let result = suggest_names(
        "abcdef",
        ["abxdxf", "abcdeg", "ABCDEF", "abcdee", "abcdef"],
        options(0.0),
    )
    .unwrap();
    assert_eq!(names(&result), ["ABCDEF", "abcdee", "abcdeg", "abxdxf"]);
    assert!(result.windows(2).all(|pair| pair[0].score >= pair[1].score));
}

#[test]
fn all_allowed_result_limits_work() {
    let candidates: Vec<String> = (b'b'..=b'z')
        .map(|letter| format!("name_a{}", char::from(letter)))
        .collect();
    for limit in 1..=20 {
        let result = suggest_names(
            "name_aa",
            candidates.iter().map(String::as_str),
            SearchOptions {
                max_results: limit,
                min_score: 0.0,
            },
        )
        .unwrap();
        assert_eq!(result.len(), limit);
    }
}

#[test]
fn invalid_limits_and_nonfinite_or_out_of_range_scores_are_typed() {
    for limit in [0, 21, usize::MAX] {
        assert_eq!(
            suggest_names(
                "",
                [],
                SearchOptions {
                    max_results: limit,
                    min_score: 0.6,
                },
            ),
            Err(SearchError::InvalidMaxResults)
        );
    }
    for min_score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.01] {
        assert_eq!(
            suggest_names("", [], options(min_score)),
            Err(SearchError::InvalidMinScore)
        );
    }
    for min_score in [-0.0, 0.0, 1.0] {
        assert!(
            suggest_names("", [], options(min_score))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn options_are_validated_before_the_iterator_is_consumed() {
    let candidates = std::iter::from_fn(|| -> Option<&'static str> {
        panic!("invalid options must not consume input")
    });
    assert_eq!(
        suggest_names("query", candidates, options(f64::NAN)),
        Err(SearchError::InvalidMinScore)
    );
}

#[test]
fn scalar_limits_accept_the_boundary_and_reject_boundary_plus_one() {
    for scalar in ['a', '😀'] {
        let boundary = scalar.to_string().repeat(MAX_NAME_CHARS);
        assert!(
            suggest_names(&boundary, [boundary.as_str()], SearchOptions::default())
                .unwrap()
                .is_empty()
        );
        assert!(
            suggest_names("", [boundary.as_str()], SearchOptions::default())
                .unwrap()
                .is_empty()
        );
        let over = format!("{boundary}{scalar}");
        assert_eq!(
            suggest_names(&over, [], SearchOptions::default()),
            Err(SearchError::QueryTooLong)
        );
        assert_eq!(
            suggest_names("", [over.as_str()], SearchOptions::default()),
            Err(SearchError::CandidateTooLong)
        );
    }
}

#[test]
fn maximum_length_names_and_expanding_lowercase_are_scored_without_truncation() {
    let prefix = "a".repeat(MAX_NAME_CHARS - 1);
    let query = format!("{prefix}b");
    let candidate = format!("{prefix}c");
    assert_eq!(
        only_score(&query, &candidate),
        1.0 - 1.0 / MAX_NAME_CHARS as f64
    );

    let expanding_prefix = "İ".repeat(MAX_NAME_CHARS - 1);
    let query = format!("{expanding_prefix}x");
    let candidate = format!("{expanding_prefix}y");
    let score = only_score(&query, &candidate);
    assert!((0.6..1.0).contains(&score));
}

#[test]
fn duplicates_and_exact_matches_count_toward_candidate_limits() {
    for candidate in ["name", "NAME"] {
        let result = suggest_names(
            "name",
            std::iter::repeat_n(candidate, MAX_CANDIDATES),
            SearchOptions::default(),
        )
        .unwrap();
        assert_eq!(result.len(), usize::from(candidate != "name"));
        assert_eq!(
            suggest_names(
                "name",
                std::iter::repeat_n(candidate, MAX_CANDIDATES + 1),
                SearchOptions::default(),
            ),
            Err(SearchError::TooManyCandidates)
        );
    }
}

#[test]
fn utf8_cumulative_budget_includes_query_duplicates_and_exact_matches() {
    let boundary_name = "😀".repeat(MAX_NAME_CHARS);
    let candidate_count = MAX_TOTAL_BYTES / boundary_name.len() - 1;
    assert_eq!(boundary_name.len() * (candidate_count + 1), MAX_TOTAL_BYTES);
    assert!(
        suggest_names(
            &boundary_name,
            std::iter::repeat_n(boundary_name.as_str(), candidate_count),
            SearchOptions::default(),
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        suggest_names(
            &boundary_name,
            std::iter::repeat_n(boundary_name.as_str(), candidate_count).chain(["x"]),
            SearchOptions::default(),
        ),
        Err(SearchError::InputTooLarge)
    );
    assert_eq!(
        suggest_names(
            "",
            std::iter::repeat_n(boundary_name.as_str(), candidate_count + 1).chain(["x"]),
            SearchOptions::default(),
        ),
        Err(SearchError::InputTooLarge)
    );
}

#[test]
fn blank_queries_and_full_results_do_not_hide_late_validation_errors() {
    let overlong = "x".repeat(MAX_NAME_CHARS + 1);
    for query in ["", " ", "name"] {
        assert_eq!(
            suggest_names(
                query,
                ["NAME", overlong.as_str()],
                SearchOptions {
                    max_results: 1,
                    min_score: 0.0,
                },
            ),
            Err(SearchError::CandidateTooLong)
        );
        assert_eq!(
            suggest_names(
                query,
                std::iter::repeat_n("", MAX_CANDIDATES + 1),
                SearchOptions::default(),
            ),
            Err(SearchError::TooManyCandidates)
        );
    }
}

#[test]
fn unicode_lowercase_expansion_and_decomposed_input_are_safe() {
    let expanded = "i\u{307}tem";
    let result = suggest_names("İtem", [expanded], options(1.0)).unwrap();
    assert_eq!(names(&result), [expanded]);
    for query in [
        "café_endpoint",
        "cafe\u{301}_endpoint",
        "猫の名前",
        "🦀🦀🦀",
    ] {
        let result = suggest_names(
            query,
            ["cafe\u{301}_endpont", "café_endpont", "猫の名札", "🦀🦀🐈"],
            options(0.0),
        )
        .unwrap();
        assert!(
            result
                .iter()
                .all(|hint| hint.score.is_finite() && (0.0..=1.0).contains(&hint.score))
        );
    }
}

#[test]
fn unicode_ties_use_original_name_order_not_enumeration() {
    let result = suggest_names("aa", ["a猫", "aβ", "aé", "aβ"], options(0.0)).unwrap();
    assert_eq!(names(&result), ["aé", "aβ", "a猫"]);
    assert!(result.iter().all(|hint| hint.score == 0.5));
}

#[test]
fn returned_names_are_borrowed_original_text_and_debug_is_redacted() {
    let candidate = String::from("na\tme");
    let result = suggest_names("na\nme", [candidate.as_str()], options(0.0)).unwrap();
    assert_eq!(names(&result), [candidate.as_str()]);
    assert!(std::ptr::eq(result[0].name.as_ptr(), candidate.as_ptr()));
    let debug = format!("{:?}", result[0]);
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("na\\tme"));
    assert!(!debug.contains("na\tme"));
    assert!(!debug.contains('\t'));
    assert!(!debug.contains('\n'));
}

#[test]
fn validation_error_display_and_debug_never_echo_input() {
    let fragment = "sensitive_name_\u{1b}[31m";
    let oversized = fragment.repeat(MAX_NAME_CHARS);
    let query_error = suggest_names(&oversized, [], SearchOptions::default()).unwrap_err();
    let candidate_error =
        suggest_names("query", [oversized.as_str()], SearchOptions::default()).unwrap_err();
    for error in [
        query_error,
        candidate_error,
        SearchError::InvalidMaxResults,
        SearchError::InvalidMinScore,
        SearchError::TooManyCandidates,
        SearchError::InputTooLarge,
    ] {
        let diagnostic = format!("{error} {error:?}");
        assert!(!diagnostic.contains("sensitive"));
        assert!(!diagnostic.contains('\u{1b}'));
        assert!(diagnostic.chars().all(|character| !character.is_control()));
        assert!(std::error::Error::source(&error).is_none());
    }
}
