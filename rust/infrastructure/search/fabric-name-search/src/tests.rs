// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{identifier_tokens, lowercase_chars, osa_distance};

#[test]
fn osa_has_known_distances_and_is_symmetric() {
    for (left, right, expected) in [
        ("", "", 0),
        ("", "abc", 3),
        ("abc", "", 3),
        ("same", "same", 0),
        ("abcd", "abxd", 1),
        ("abcd", "abc", 1),
        ("abcd", "abcde", 1),
        ("abcd", "abdc", 1),
        ("abcdef", "badcfe", 3),
        ("kitten", "sitting", 3),
        ("CA", "ABC", 3),
        ("é猫🦀", "猫é🦀", 1),
        ("café", "cafe\u{301}", 2),
    ] {
        let left: Vec<char> = left.chars().collect();
        let right: Vec<char> = right.chars().collect();
        assert_eq!(osa_distance(&left, &right, 10), Some(expected));
        assert_eq!(osa_distance(&right, &left, 10), Some(expected));
        assert_eq!(osa_distance(&left, &right, expected), Some(expected));
        if expected > 0 {
            assert_eq!(osa_distance(&left, &right, expected - 1), None);
        }
    }
}

#[test]
fn banded_distance_matches_full_distance_for_small_alphabet() {
    let mut words = vec![Vec::new()];
    for length in 1..=4 {
        for bits in 0..(1 << length) {
            words.push(
                (0..length)
                    .map(|position| {
                        if bits & (1 << position) == 0 {
                            'a'
                        } else {
                            'b'
                        }
                    })
                    .collect(),
            );
        }
    }
    for left in &words {
        for right in &words {
            let exact = osa_distance(left, right, 4).unwrap();
            for budget in 0..=4 {
                assert_eq!(
                    osa_distance(left, right, budget),
                    (exact <= budget).then_some(exact)
                );
            }
        }
    }
}

#[test]
fn tokenization_handles_camel_acronyms_separators_and_combining_marks() {
    for name in [
        "production_endpoint",
        "production-endpoint",
        "productionEndpoint",
        "ProductionEndpoint",
    ] {
        assert_eq!(
            identifier_tokens(name),
            vec![lowercase_chars("production"), lowercase_chars("endpoint")]
        );
    }
    assert_eq!(
        identifier_tokens("HTTPServer"),
        vec![lowercase_chars("http"), lowercase_chars("server")]
    );
    assert_eq!(
        identifier_tokens("cafe\u{301}Endpoint"),
        vec![lowercase_chars("cafe\u{301}"), lowercase_chars("endpoint")]
    );
}
