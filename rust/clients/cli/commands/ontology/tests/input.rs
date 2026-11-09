// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Strict local JSON/media/byte-bound contracts, without host or credentials.

#![deny(unnameable_test_items)]

use fabric_cli_ontology::{InputError, decode_json_input};
use serde_json::json;
use std::num::NonZeroUsize;

#[test]
fn omitted_empty_invalid_utf8_and_unsupported_media_are_distinct() {
    let bound = NonZeroUsize::new(1024).unwrap();
    assert_eq!(
        decode_json_input(None, "application/json", bound),
        Err(InputError::Missing)
    );
    for bytes in [b"".as_slice(), b" \n\t".as_slice()] {
        assert_eq!(
            decode_json_input(Some(bytes), "application/json", bound),
            Err(InputError::Empty)
        );
    }
    assert_eq!(
        decode_json_input(Some(&[0xff]), "application/json", bound),
        Err(InputError::InvalidUtf8)
    );
    for media in [
        "text/plain",
        "application/merge-patch+json",
        "application/xml",
        "application/json; charset=utf-8",
    ] {
        assert_eq!(
            decode_json_input(Some(b"{}"), media, bound),
            Err(InputError::UnsupportedMedia)
        );
    }
}

#[test]
fn exact_input_size_is_admitted_and_one_extra_byte_is_rejected_before_decode() {
    let bound = NonZeroUsize::new(2).unwrap();
    assert_eq!(
        decode_json_input(Some(b"{}"), "application/json", bound),
        Ok(json!({}))
    );
    assert_eq!(
        decode_json_input(Some(b"{} "), "application/json", bound),
        Err(InputError::TooLarge)
    );
}

#[test]
fn duplicate_keys_and_escape_equivalent_nested_keys_are_rejected() {
    let bound = NonZeroUsize::new(1024).unwrap();
    for bytes in [
        br#"{"name":"a","name":"b"}"#.as_slice(),
        br#"{"body":{"a":1,"\u0061":2}}"#.as_slice(),
        br#"{"parts":[{"path":"a","path":"b"}]}"#.as_slice(),
    ] {
        let error = decode_json_input(Some(bytes), "application/json", bound).unwrap_err();
        assert_eq!(error, InputError::InvalidJson);
        assert!(!error.to_string().contains("name"));
    }
}

#[test]
fn one_object_is_required_and_partial_documents_never_succeed() {
    let bound = NonZeroUsize::new(4096).unwrap();
    for bytes in [
        b"{}{}".as_slice(),
        b"[]",
        b"null",
        b"true",
        b"1",
        b"\"text\"",
        b"{\"x\":1} trailing",
        b"{\"x\":NaN}",
        b"{\"x\":1e999}",
        b"{\"x\":",
    ] {
        assert_eq!(
            decode_json_input(Some(bytes), "application/json", bound),
            Err(InputError::InvalidJson)
        );
    }
    let deep = format!("{{\"x\":{}0{}}}", "[".repeat(200), "]".repeat(200));
    assert_eq!(
        decode_json_input(Some(deep.as_bytes()), "application/json", bound),
        Err(InputError::InvalidJson)
    );
}

#[test]
fn scalar_types_null_omission_arrays_unicode_and_unknown_fields_are_preserved() {
    let bytes = br#"{"omittedSibling":{},"null":null,"integer":9007199254740993,"negative":-2,"fraction":1.5,"yes":true,"text":"\u03bb","array":[null,{"future":7}]}"#;
    let bound = NonZeroUsize::new(bytes.len()).unwrap();
    let value = decode_json_input(Some(bytes), "application/json", bound).unwrap();
    assert_eq!(value["integer"].as_u64(), Some(9_007_199_254_740_993));
    assert_eq!(value["negative"].as_i64(), Some(-2));
    assert_eq!(value["fraction"].as_f64(), Some(1.5));
    assert!(value["null"].is_null());
    assert!(value["omittedSibling"].get("null").is_none());
    assert_eq!(value["text"], "\u{03bb}");
    assert_eq!(value["array"], json!([null, {"future":7}]));
}
