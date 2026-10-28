// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive actual-source receipts; no released oracle expectations are changed.

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_deploy::definitions::{
    DefinitionLimits, ExportStorage, decode_export_files_for_storage,
};
use serde_json::Value;

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
        let mut expected = STANDARD
            .decode(case["local_base64"].as_str().unwrap())
            .unwrap();
        #[cfg(not(windows))]
        if case["name"] != "remote-binary" {
            expected = expected
                .split(|_| false)
                .flat_map(|bytes| {
                    let mut out = Vec::new();
                    let mut index = 0;
                    while index < bytes.len() {
                        if bytes[index..].starts_with(b"\r\n") {
                            index += 1;
                        }
                        out.push(bytes[index]);
                        index += 1;
                    }
                    out
                })
                .collect();
        }
        #[cfg(windows)]
        let _ = &mut expected;
        assert_eq!(files[0].path, case["path"].as_str().unwrap());
        assert_eq!(files[0].bytes, expected, "{}", case["name"]);
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
