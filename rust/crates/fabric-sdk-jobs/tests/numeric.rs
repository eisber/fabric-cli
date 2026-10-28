// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent Unicode decimal inventory and checked lexical conversion.

use fabric_sdk_jobs::{source_decimal_digit, source_integer_text};
use serde_json::Value;

#[test]
fn decimal_inventory_matches_the_actual_python_unicode_database_without_extra_numerics() {
    let capture: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/jobs/numeric_source.json"
    ))
    .unwrap();
    let expected: Vec<_> = capture["decimal_blocks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|start| {
            let start = u32::try_from(start.as_u64().unwrap()).unwrap();
            (0..10_u8).map(move |digit| (start + u32::from(digit), digit))
        })
        .collect();
    let actual: Vec<_> = (0..=0x10ffff)
        .filter_map(|value| {
            char::from_u32(value)
                .and_then(|ch| source_decimal_digit(ch).map(|digit| (value, digit)))
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn lexical_normalization_preserves_exact_integer_bounds_and_rejects_non_decimal_syntax() {
    for (raw, expected) in [
        (" +１_２ ", "+12"),
        ("-٩_٢", "-92"),
        (
            "１８４４６７４４０７３７０９５５１６１５",
            "18446744073709551615",
        ),
        (
            "-９２２３３７２０３６８５４７７５８０８",
            "-9223372036854775808",
        ),
    ] {
        assert_eq!(source_integer_text(raw).as_deref(), Some(expected));
    }
    for invalid in ["", "+", "_1", "1_", "1__2", "²", "½", "一", "−1", "１.０"] {
        assert!(source_integer_text(invalid).is_none(), "{invalid}");
    }
    let maximum = source_integer_text("１８４４６７４４０７３７０９５５１６１５").unwrap();
    assert_eq!(maximum.parse::<u64>().unwrap(), u64::MAX);
    assert!(
        source_integer_text("18446744073709551616")
            .unwrap()
            .parse::<u64>()
            .is_err()
    );
}
