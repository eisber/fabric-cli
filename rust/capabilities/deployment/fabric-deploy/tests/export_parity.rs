// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

//! Additive actual-source receipts; no released oracle expectations are changed.

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_deploy::definitions::{
    DefinitionLimits, ExportStorage, decode_export_files_for_storage,
};
use serde_json::Value;

#[test]
fn noncanonical_base64_source_domain_is_explicitly_rejected_not_wrong_byte_fallback() {
    let receipt: Value =
        serde_json::from_str(include_str!("definition_export_base64_source.json")).unwrap();
    assert_eq!(
        receipt["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    for case in receipt["cases"].as_array().unwrap() {
        let result = decode_export_files_for_storage(
            &case["input"],
            false,
            DefinitionLimits::default(),
            ExportStorage::Local,
        );
        if case["name"] == "canonical" || case["name"] == "source-error-original-payload" {
            let files = result.unwrap();
            assert_eq!(
                STANDARD.encode(&files[0].bytes),
                case["expected_base64"].as_str().unwrap()
            );
        } else {
            assert!(
                case["source_part_error"].is_null(),
                "Python accepted this domain"
            );
            let error = result.unwrap_err();
            assert!(
                matches!(
                    error.kind,
                    fabric_deploy::definitions::DefinitionErrorKind::InvalidDefinition
                ),
                "{}",
                case["name"]
            );
            assert!(error.completed_paths.is_empty());
            assert!(error.incomplete_path.is_none());
        }
    }
}

fn expected(encoded: &Value, binary: bool) -> Vec<u8> {
    let bytes = STANDARD.decode(encoded.as_str().unwrap()).unwrap();
    #[cfg(windows)]
    {
        let _ = binary;
        bytes
    }
    #[cfg(not(windows))]
    {
        if binary {
            return bytes;
        }
        let mut output = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index..].starts_with(b"\r\n") {
                index += 1;
            }
            output.push(bytes[index]);
            index += 1;
        }
        output
    }
}

#[test]
fn actual_python_fallback_and_remote_storage_captures_match() {
    let receipt: Value =
        serde_json::from_str(include_str!("definition_export_fallback_source.json")).unwrap();
    assert_eq!(
        receipt["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    for case in receipt["cases"].as_array().unwrap() {
        let files = decode_export_files_for_storage(
            &case["input"],
            false,
            DefinitionLimits::default(),
            ExportStorage::Local,
        )
        .unwrap();
        assert_eq!(files[0].path, case["path"].as_str().unwrap());
        assert_eq!(
            files[0].bytes,
            expected(&case["local_base64"], case["name"] == "remote-binary"),
            "{}",
            case["name"]
        );
        if let Some(remote) = case["remote"].as_array() {
            let files = decode_export_files_for_storage(
                &case["input"],
                false,
                DefinitionLimits::default(),
                ExportStorage::Lakehouse,
            )
            .unwrap();
            assert_eq!(
                STANDARD.encode(&files[0].bytes),
                remote[1]["base64"].as_str().unwrap()
            );
            assert_eq!(
                files[0].bytes.len(),
                remote[2]["position"].as_u64().unwrap() as usize
            );
        }
    }
}

#[test]
fn actual_python_wide_integer_nonfinite_and_notebook_captures_match() {
    let receipt: Value =
        serde_json::from_str(include_str!("definition_export_native_source.json")).unwrap();
    assert_eq!(
        receipt["source"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    for case in receipt["cases"].as_array().unwrap() {
        let files = decode_export_files_for_storage(
            &case["input"],
            case["notebook"].as_bool().unwrap(),
            DefinitionLimits::default(),
            ExportStorage::Local,
        )
        .unwrap();
        assert_eq!(
            files[0].bytes,
            expected(&case["expected_base64"], false),
            "{}",
            case["name"]
        );
    }
}
