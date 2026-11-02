// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use crate::text::Presentation;
use serde_json::json;

// Generated only after the controller grants the full-scalar CPU slot.
const SOURCE_DATA: &str = include_str!("source-data.json");
const ORACLE: &str = include_str!("oracle.json");

fn provider() -> ProductionUnicode {
    ProductionUnicode::from_json(SOURCE_DATA, UnicodeLimits::default()).unwrap()
}

fn oracle() -> Value {
    serde_json::from_str(ORACLE).unwrap()
}

#[test]
fn every_valid_scalar_matches_the_independent_python_checksum() {
    let provider = provider();
    let mut checksum = 0xcbf2_9ce4_8422_2325_u64;
    let mut covered = 0;
    let mut category_differences = 0;
    let mut width_differences = 0;
    for point in (0..0xd800).chain(0xe000..0x110000) {
        let character = char::from_u32(point).unwrap();
        let source = provider.properties(character);
        let flags = u8::from(source.printable)
            | (u8::from(source.alphanumeric) << 1)
            | (u8::from(source.uppercase) << 2)
            | (u8::from(source.cased) << 3)
            | (u8::from(source.case_ignorable) << 4)
            | (u8::from(source.whitespace) << 5);
        let properties = [
            EAW.iter()
                .position(|value| *value == source.east_asian_width)
                .unwrap() as u8,
            CATEGORIES
                .iter()
                .position(|value| *value == source.general_category)
                .unwrap() as u8,
            flags,
            source.rich_width as u8,
        ];
        for byte in point.to_le_bytes().into_iter().chain(properties) {
            checksum = (checksum ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
        assert_eq!(provider.width(character), Some(source.source_width));
        assert!((1..=2).contains(&source.source_width));
        assert!(source.rich_width <= 2);
        let icu = provider.icu_properties(character);
        let source_category = match source.general_category {
            "Cn" => GeneralCategory::Unassigned,
            "Lu" => GeneralCategory::UppercaseLetter,
            "Ll" => GeneralCategory::LowercaseLetter,
            "Lt" => GeneralCategory::TitlecaseLetter,
            "Lm" => GeneralCategory::ModifierLetter,
            "Lo" => GeneralCategory::OtherLetter,
            "Mn" => GeneralCategory::NonspacingMark,
            "Me" => GeneralCategory::EnclosingMark,
            "Mc" => GeneralCategory::SpacingMark,
            "Nd" => GeneralCategory::DecimalNumber,
            "Nl" => GeneralCategory::LetterNumber,
            "No" => GeneralCategory::OtherNumber,
            "Zs" => GeneralCategory::SpaceSeparator,
            "Zl" => GeneralCategory::LineSeparator,
            "Zp" => GeneralCategory::ParagraphSeparator,
            "Cc" => GeneralCategory::Control,
            "Cf" => GeneralCategory::Format,
            "Co" => GeneralCategory::PrivateUse,
            "Cs" => panic!("a valid scalar must not have surrogate category"),
            "Pd" => GeneralCategory::DashPunctuation,
            "Ps" => GeneralCategory::OpenPunctuation,
            "Pe" => GeneralCategory::ClosePunctuation,
            "Pc" => GeneralCategory::ConnectorPunctuation,
            "Po" => GeneralCategory::OtherPunctuation,
            "Sm" => GeneralCategory::MathSymbol,
            "Sc" => GeneralCategory::CurrencySymbol,
            "Sk" => GeneralCategory::ModifierSymbol,
            "So" => GeneralCategory::OtherSymbol,
            "Pi" => GeneralCategory::InitialPunctuation,
            "Pf" => GeneralCategory::FinalPunctuation,
            _ => panic!("unvalidated category"),
        };
        category_differences += usize::from(source_category != icu.general_category);
        let icu_width = if matches!(
            icu.east_asian_width,
            EastAsianWidth::Wide | EastAsianWidth::Fullwidth
        ) {
            2
        } else {
            1
        };
        width_differences += usize::from(source.source_width != icu_width);
        covered += 1;
    }
    assert_eq!(covered, 1_112_064);
    assert_eq!(format!("{checksum:016x}"), provider.scalar_checksum());
    // This is a version-difference inventory, not permission to change source data.
    eprintln!(
        "all {covered} scalars: CPython UCD15.1 / ICU UCD18 category differences={category_differences}, F/W-width differences={width_differences}"
    );
    assert!(category_differences > 0);
}

#[test]
fn adversarial_whole_sequences_match_actual_python_and_rich() {
    let provider = provider();
    for case in oracle()["texts"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        assert_eq!(
            provider.source_text_width(text).unwrap(),
            case["source_width"].as_u64().unwrap() as usize,
            "{text:?}"
        );
        assert_eq!(
            provider.rich_text_width(text).unwrap(),
            case["rich_width"].as_u64().unwrap() as usize,
            "{text:?}"
        );
        assert_eq!(
            provider.python_string(&json!([text])).unwrap(),
            format!("[{}]", case["repr"].as_str().unwrap()),
            "{text:?}"
        );
    }
}

#[test]
fn keys_preserve_expansions_validation_and_contextual_sigma() {
    let provider = provider();
    let oracle = oracle();
    for case in oracle["keys"]
        .as_array()
        .unwrap()
        .iter()
        .chain(oracle["casing_keys"].as_array().unwrap())
    {
        let key = case["key"].as_str().unwrap();
        let observed = provider.title_key(key);
        if let Some(expected) = case["expected"].as_str() {
            assert_eq!(observed.unwrap(), expected, "{key:?}");
        } else {
            assert_eq!(
                observed.unwrap_err().to_string(),
                case["error"].as_str().unwrap(),
                "{key:?}"
            );
        }
    }
}

#[test]
fn json_domain_str_repr_and_numbers_match_python_without_changing_json() {
    let provider = provider();
    for case in oracle()["values"].as_array().unwrap() {
        assert_eq!(
            provider.python_string(&case["value"]).unwrap(),
            case["expected"].as_str().unwrap(),
            "{:?}",
            case["value"]
        );
    }
    // These are pre-existing serializer contracts, not new Unicode expectations.
    let value = json!({"é": ["\u{200b}", "\0"]});
    let before = crate::json_dumps(&value).unwrap();
    provider.python_string(&value).unwrap();
    assert_eq!(crate::json_dumps(&value).unwrap(), before);
}

#[test]
fn actual_table_renderer_consumes_the_scalar_provider() {
    let provider = provider();
    for fixture in oracle()["tables"].as_array().unwrap() {
        let case = &fixture["case"];
        let input = &case["input"];
        let fields = vec!["column".to_owned()];
        let plan = text::render_table(
            &input["data"],
            &fields,
            input["show_headers"] == true,
            case["width"].as_u64().unwrap() as usize,
            &provider,
            Presentation::default(),
        )
        .unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        plan.write_to(&mut stdout, &mut stderr).unwrap();
        assert_eq!(
            json!({
                "name": case["name"],
                "stdout": String::from_utf8(stdout).unwrap(),
                "stderr": String::from_utf8(stderr).unwrap(),
                "exit": plan.exit_code,
            }),
            fixture["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn scalar_domain_and_width_policies_remain_explicit() {
    let provider = provider();
    for point in oracle()["invalid_scalars"].as_array().unwrap() {
        let point = point.as_u64().unwrap() as u32;
        assert!(matches!(
            provider.codepoint(point),
            Err(UnicodeError::InvalidScalar(actual)) if actual == point
        ));
    }
    for point in [0, 0x378, 0xe000, 0xfdd0, 0xffff, 0x10ffff] {
        assert!(provider.codepoint(point).is_ok());
    }
    assert_eq!(provider.source_text_width("\0").unwrap(), 1);
    assert_eq!(provider.rich_text_width("\0").unwrap(), 0);
    assert_eq!(provider.source_text_width("e\u{301}").unwrap(), 2);
    assert_eq!(provider.rich_text_width("e\u{301}").unwrap(), 1);
    assert_eq!(provider.source_text_width("👩‍💻").unwrap(), 5);
    assert_eq!(provider.rich_text_width("👩‍💻").unwrap(), 2);
    assert!(provider.icu_properties('\u{200d}').default_ignorable);
    assert_eq!(provider.properties('\u{200d}').source_width, 1);
    assert_eq!(provider.properties('\u{1fae9}').general_category, "Cn");
    assert_eq!(
        provider.icu_properties('\u{1fae9}').general_category,
        GeneralCategory::OtherSymbol
    );
}

#[test]
fn incomplete_or_corrupt_tables_fail_closed() {
    let original: Value = serde_json::from_str(SOURCE_DATA).unwrap();
    for mutation in 0..6 {
        let mut data = original.clone();
        match mutation {
            0 => data["scalar_count"] = json!(27),
            1 => data["python_ucd"] = json!("18.0.0"),
            2 => {
                data["ranges"].as_array_mut().unwrap().pop();
            }
            3 => data["ranges"][0][0] = json!(1),
            4 => data["ranges"][0][2] = json!(6),
            _ => data["rich_narrow_to_wide"] = json!([0xd800]),
        }
        assert!(
            ProductionUnicode::from_json(&data.to_string(), UnicodeLimits::default()).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn coverage_validation_rejects_gaps_in_noncharacter_and_unassigned_slices() {
    let original: Value = serde_json::from_str(SOURCE_DATA).unwrap();
    for point in [0_u64, 0x378, 0xe000, 0xfdd0, 0xffff, 0x10ffff] {
        let mut data = original.clone();
        let ranges = data["ranges"].as_array_mut().unwrap();
        let index = ranges
            .iter()
            .position(|range| {
                range[0].as_u64().unwrap() <= point && range[1].as_u64().unwrap() >= point
            })
            .unwrap();
        ranges.remove(index);
        assert!(matches!(
            ProductionUnicode::from_json(&data.to_string(), UnicodeLimits::default()),
            Err(UnicodeDataError::Invalid(
                UnicodeDataKind::ScalarCoverage | UnicodeDataKind::IncompleteDomain
            ))
        ));
    }
}

#[test]
fn operations_have_finite_input_output_node_and_depth_budgets() {
    let limited = |limits| ProductionUnicode::from_json(SOURCE_DATA, limits).unwrap();
    let provider = limited(UnicodeLimits {
        input_bytes: 8,
        output_bytes: 12,
        depth: 2,
        nodes: 2,
    });
    assert!(matches!(
        provider.source_text_width("123456789"),
        Err(UnicodeError::Limit(UnicodeResourceKind::InputBytes))
    ));
    assert!(matches!(
        provider.title_key("long_name"),
        Err(UnicodeError::Limit(UnicodeResourceKind::InputBytes))
    ));
    assert!(matches!(
        provider.python_string(&json!([1, 2])),
        Err(UnicodeError::Limit(UnicodeResourceKind::Nodes))
    ));
    assert!(matches!(
        limited(UnicodeLimits {
            depth: 1,
            ..UnicodeLimits::default()
        })
        .python_string(&json!([[0]])),
        Err(UnicodeError::Limit(UnicodeResourceKind::Depth))
    ));
    assert!(matches!(
        provider.python_string(&json!(["\u{10ffff}"])),
        Err(UnicodeError::Limit(UnicodeResourceKind::OutputBytes))
    ));
    assert!(matches!(
        limited(UnicodeLimits {
            input_bytes: 8,
            ..UnicodeLimits::default()
        })
        .python_string(&json!(["12345", "6789"])),
        Err(UnicodeError::Limit(UnicodeResourceKind::InputBytes))
    ));
    assert!(
        ProductionUnicode::from_json(
            SOURCE_DATA,
            UnicodeLimits {
                input_bytes: usize::MAX,
                ..UnicodeLimits::default()
            }
        )
        .is_err()
    );
}

#[test]
fn embedded_callbacks_use_one_immutable_complete_provider() {
    let first = embedded_provider().unwrap();
    assert!(std::ptr::eq(first, embedded_provider().unwrap()));
    assert!(std::ptr::eq(first, width_provider().unwrap()));
    assert_eq!(key_formatter("état_id").unwrap(), "État ID");
    assert_eq!(
        value_formatter(&json!(["é", "\u{200b}", "\u{a0}"])).unwrap(),
        "['é', '\\u200b', '\\xa0']"
    );
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(move || {
                assert!(std::ptr::eq(first, embedded_provider().unwrap()));
                assert_eq!(first.codepoint(0x10ffff).unwrap().general_category, "Cn");
            });
        }
    });
}

#[test]
fn initialization_failures_remain_typed_errors_without_fallback_or_panic() {
    use std::error::Error;

    let failed = LazyLock::new(|| ProductionUnicode::from_json("{", UnicodeLimits::default()));
    for _ in 0..2 {
        let error = failed.as_ref().unwrap_err();
        assert!(matches!(error, UnicodeDataError::Json(_)));
        assert!(error.source().is_some());
        assert_eq!(
            error.to_text_error(),
            TextError::UnicodeData(UnicodeDataKind::MalformedJson)
        );
        assert_eq!(error.to_text_error().command_error().code, "UnicodeData");
    }
    let oversized = " ".repeat(MAX_DATA_BYTES + 1);
    let error = ProductionUnicode::from_json(&oversized, UnicodeLimits::default()).unwrap_err();
    assert!(matches!(
        error,
        UnicodeDataError::Resource(UnicodeResourceKind::DataBytes)
    ));
    assert_eq!(
        error.to_text_error(),
        TextError::UnicodeResource(UnicodeResourceKind::DataBytes)
    );
}

#[test]
fn operation_errors_preserve_source_validation_and_partial_output() {
    let source_error = text::render_key_values(
        &json!({"id": 1, "bad-key": 2}),
        text::title_key,
        text::python_string,
        Presentation::default(),
    )
    .unwrap_err();
    let partial = source_error.partial_output().unwrap().clone();
    let original_message = source_error.to_string();
    let error = TextError::from(UnicodeError::Text(source_error));
    assert_eq!(error.partial_output(), Some(&partial));
    assert_eq!(error.to_string(), original_message);
    assert_eq!(error.command_error().code, "UnexpectedError");
    assert_eq!(
        TextError::from(UnicodeError::Limit(UnicodeResourceKind::OutputBytes)),
        TextError::UnicodeResource(UnicodeResourceKind::OutputBytes)
    );
    assert_eq!(
        TextError::UnicodeResource(UnicodeResourceKind::OutputBytes)
            .command_error()
            .code,
        "UnicodeResourceLimit"
    );
}

fn observe_defaults(case: &Value) -> Value {
    let input = &case["input"];
    let envelope = crate::OutputEnvelope::new(
        crate::Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        crate::OutputInput {
            command: input["command"].as_str().map(str::to_owned),
            data: input.get("data").cloned(),
            ..crate::OutputInput::default()
        },
    );
    let result = text::render(
        &envelope,
        text::OutputFormat::Text,
        &text::TextOptions {
            key_value: input["key_value"] == true,
            show_headers: input["show_headers"] == true,
            terminal_width: Some(case["width"].as_u64().unwrap() as usize),
            ..text::TextOptions::default()
        },
        width_provider().unwrap(),
        Presentation::default(),
    );
    let (plan, mut observed) = match &result {
        Ok(plan) => (
            Some(plan),
            json!({"name":case["name"],"exit":plan.exit_code}),
        ),
        Err(error) => (
            error.partial_output(),
            json!({
                "name":case["name"],"error":error.to_string(),
                "code":error.command_error().code,
            }),
        ),
    };
    if let Some(plan) = plan {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        plan.write_to(&mut stdout, &mut stderr).unwrap();
        observed["stdout"] = json!(String::from_utf8(stdout).unwrap());
        observed["stderr"] = json!(String::from_utf8(stderr).unwrap());
    }
    observed
}

#[test]
fn renderer_defaults_match_source_unicode_and_partial_output() {
    let options = text::TextOptions::default();
    assert_eq!((options.key_formatter)("état_id").unwrap(), "État ID");
    assert_eq!(
        (options.value_formatter)(&json!(["\u{200b}"])).unwrap(),
        "['\\u200b']"
    );
    for fixture in oracle()["render_cases"].as_array().unwrap() {
        assert_eq!(
            observe_defaults(&fixture["case"]),
            fixture["expected"],
            "{}",
            fixture["case"]["name"]
        );
    }
}

#[test]
fn default_callbacks_report_typed_resource_limits_without_changing_json() {
    let value = Value::String("x".repeat(UnicodeLimits::default().input_bytes + 1));
    assert_eq!(
        value_formatter(&value),
        Err(TextError::UnicodeResource(UnicodeResourceKind::InputBytes))
    );
    let envelope = crate::OutputEnvelope::new(
        crate::Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        crate::OutputInput {
            data: Some(value),
            ..crate::OutputInput::default()
        },
    );
    let expected = envelope.render(crate::Indent::Four).unwrap();
    let observed = text::render(
        &envelope,
        text::OutputFormat::Json,
        &text::TextOptions::default(),
        width_provider().unwrap(),
        Presentation::default(),
    )
    .unwrap();
    assert_eq!(observed.records, vec![expected]);
}
