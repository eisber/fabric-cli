// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::WORKSPACE;
use crate::family_support::Harness;
use fabric_cli::families::transfer_files::TokioTransferFiles;
use fabric_cli_transfer::TransferFiles;
use fabric_deploy::definitions::{DefinitionLimits, ImportTarget};
use serde_json::{Value, json};

fn target() -> ImportTarget<'static> {
    ImportTarget {
        workspace_id: WORKSPACE,
        item_id: None,
        item_type: "DataPipeline",
        display_name: "Synthetic",
        folder_id: None,
    }
}

#[tokio::test]
async fn selected_import_roots_bind_only_exact_source_alias_on_the_actual_worker() {
    for (file, deferred) in [(false, false), (true, false), (false, true), (true, true)] {
        let host = Harness::new();
        let source = host
            .root
            .join(if file { "selected.bin" } else { "selected" });
        if file {
            std::fs::write(&source, b"not traversed by os.walk").unwrap();
        } else {
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("b.whl"), [0, 255, 13, 10]).unwrap();
        }
        let alias = format!("~{}selected", std::path::MAIN_SEPARATOR);
        let limits = DefinitionLimits::default();
        let mut files = if deferred {
            TokioTransferFiles::defer_import_root(
                source.clone(),
                alias.clone(),
                limits,
                tokio::runtime::Handle::current(),
            )
        } else {
            TokioTransferFiles::for_import_root(
                source.clone(),
                alias.clone(),
                limits,
                tokio::runtime::Handle::current(),
            )
            .await
            .unwrap()
        };
        assert_eq!(files.resolve_import_async(&alias).await.unwrap(), source);
        assert!(files.resolve_import_async("another-input").await.is_err());
        assert!(files.resolve_directory_async(&alias).await.is_err());
        let prepared = files
            .prepare_import_async(&source, target(), None, limits)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(prepared.body()).unwrap();
        assert_eq!(
            body["definition"]["parts"],
            if file {
                json!([])
            } else {
                json!([{"path":"b.whl","payload":"AP8NCg==","payloadType":"InlineBase64"}])
            }
        );
        assert!(host.requests().is_empty());
    }
}

#[tokio::test]
async fn deferred_import_waits_for_matching_resolution_without_a_cwd_preflight() {
    let host = Harness::new();
    let source = host.root.join("created-after-construction");
    let limits = DefinitionLimits::default();
    let mut files = TokioTransferFiles::defer_import_root(
        source.clone(),
        "explicit-input".into(),
        limits,
        tokio::runtime::Handle::current(),
    );
    assert!(
        files
            .prepare_import_async(&source, target(), None, limits)
            .await
            .is_err()
    );
    assert_eq!(
        files
            .resolve_import_async("different-input")
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("b.whl"), [0, 255, 13, 10]).unwrap();
    assert_eq!(
        files.resolve_import_async("explicit-input").await.unwrap(),
        source
    );
    let prepared = files
        .prepare_import_async(&source, target(), None, limits)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(prepared.body()).unwrap();
    assert_eq!(body["definition"]["parts"][0]["payload"], "AP8NCg==");
    assert!(host.requests().is_empty());
}

#[tokio::test]
async fn selected_file_parent_does_not_grant_import_reads_for_another_source() {
    let host = Harness::new();
    let source = host.root.join("selected.bin");
    let other = host.root.join("other");
    std::fs::write(&source, b"ignored").unwrap();
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("part.txt"), b"must not be read").unwrap();
    let limits = DefinitionLimits::default();
    let mut files = TokioTransferFiles::for_import_root(
        source,
        "selected.bin".into(),
        limits,
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    assert!(
        files
            .prepare_import_async(&other, target(), None, limits)
            .await
            .is_err()
    );
    assert!(host.requests().is_empty());
}

#[tokio::test]
async fn selected_source_aliases_do_not_bypass_approved_unsafe_path_domains() {
    let host = Harness::new();
    let source = host.root.join("selected");
    std::fs::create_dir(&source).unwrap();
    for alias in [
        "../outside",
        "..\\outside",
        "\\\\server\\share",
        "//server/share",
        "https://example.invalid/input",
        "NUL",
        "file.bin:stream",
    ] {
        assert!(
            TokioTransferFiles::for_import_root(
                source.clone(),
                alias.into(),
                DefinitionLimits::default(),
                tokio::runtime::Handle::current(),
            )
            .await
            .is_err(),
            "{alias}"
        );
    }
    assert!(host.requests().is_empty());
}
