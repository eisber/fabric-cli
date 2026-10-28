// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Deferred worker activation through real root export/storage composition.

#![deny(unnameable_test_items)]

use super::{
    NOTEBOOK, PIPELINE, WS, definition, host, injected_export_with_http, items, remote_metadata,
    success,
};
use fabric_cli::families::{FamilyEvidence, transfer_files::TokioTransferFiles};
use fabric_cli_transfer::TransferFiles;
use serde_json::{Value, json};
use std::{fs, future::pending, io, path::PathBuf};

#[tokio::test]
async fn deferred_directory_waits_for_exact_export_resolution_and_preserves_its_alias() {
    let host = host();
    let selected = host.root.join("created-after-construction");
    let alias = format!("~{}selected", std::path::MAIN_SEPARATOR);
    let mut files = TokioTransferFiles::defer_export_root(
        selected.clone(),
        alias.clone(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    assert!(files.resolve_directory_async(&alias).await.is_err());
    assert!(files.resolve_import_async(&alias).await.is_err());
    assert_eq!(
        files
            .resolve_export_directory_async("different-alias")
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!selected.exists());
    fs::create_dir(&selected).unwrap();
    for _ in 0..3 {
        assert_eq!(
            files.resolve_export_directory_async(&alias).await.unwrap(),
            selected
        );
        assert!(files.is_empty_async(&selected).await.unwrap());
    }
}

#[tokio::test]
async fn deferred_export_keeps_directory_only_and_unsafe_candidate_checks() {
    let host = host();
    let file = host.root.join("not-a-directory");
    fs::write(&file, b"not a directory").unwrap();
    let mut files = TokioTransferFiles::defer_export_root(
        file,
        "output".into(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    assert!(
        files
            .resolve_export_directory_async("output")
            .await
            .is_err()
    );
    for selected in [
        host.root.join("..").join("outside"),
        host.root.join("missing"),
        PathBuf::from("\\\\server\\share\\output"),
    ] {
        let mut files = TokioTransferFiles::defer_export_root(
            selected.clone(),
            "output".into(),
            Default::default(),
            tokio::runtime::Handle::current(),
        );
        assert!(
            files
                .resolve_export_directory_async("output")
                .await
                .is_err()
        );
        assert_eq!(
            files.is_empty_async(&selected).await.unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
}

#[tokio::test]
async fn real_remote_export_does_not_activate_the_nonexistent_local_candidate() {
    let mut host = host();
    let selected = host.root.join("must-not-exist");
    let alias = "Demo.Workspace/Lake.Lakehouse/Files";
    let mut files = TokioTransferFiles::defer_export_root(
        selected.clone(),
        alias.into(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    remote_metadata(&host, definition("part.txt", "eA=="));
    host.reply(201, Value::Null);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let http = host.http.clone();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &[
            "fab-rs",
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            alias,
        ],
        http,
    )
    .await;
    success(&output);
    assert!(!selected.exists());
    assert_eq!(
        files.is_empty_async(&selected).await.unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(host.requests().iter().any(|request| {
        request["url"]
            .as_str()
            .unwrap()
            .contains("onelake.dfs.fabric.microsoft.com")
    }));
}

#[tokio::test]
async fn repeated_root_workspace_exports_keep_the_selected_directory_and_alias() {
    let mut host = host();
    let selected = host.root.join("selected");
    fs::create_dir(&selected).unwrap();
    let mut files = TokioTransferFiles::defer_export_root(
        selected.clone(),
        "output".into(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    for _ in 0..2 {
        host.metadata_with_id(WS, items());
        host.reply(200, json!({"id":NOTEBOOK}));
        host.reply(
            200,
            definition("notebook-content.ipynb", "eyJjZWxscyI6W119"),
        );
        host.reply(200, json!({"id":PIPELINE}));
        host.reply(200, definition("pipeline-content.json", "e30="));
        let http = host.http.clone();
        let output = injected_export_with_http(
            &mut host,
            &mut files,
            pending(),
            &[
                "fab-rs",
                "export",
                "Demo.Workspace",
                "--all",
                "-f",
                "-o",
                "output",
            ],
            http,
        )
        .await;
        success(&output);
        assert!(
            matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
            if result.exported_ids == [NOTEBOOK, PIPELINE])
        );
        assert_eq!(
            files
                .resolve_export_directory_async("output")
                .await
                .unwrap(),
            selected
        );
    }
    assert!(
        selected
            .join("zulu.Notebook")
            .join("notebook-content.ipynb")
            .is_file()
    );
    assert!(
        selected
            .join("alpha.DataPipeline")
            .join("pipeline-content.json")
            .is_file()
    );
    assert!(
        fs::read_dir(host.root.join("output"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn real_bulk_export_activates_the_same_deferred_directory_capability() {
    let mut host = host();
    let selected = host.root.join("selected-bulk");
    fs::create_dir(&selected).unwrap();
    let mut files = TokioTransferFiles::defer_export_root(
        selected.clone(),
        "output".into(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    host.metadata_with_id(
        WS,
        json!([{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]),
    );
    host.reply(200, json!({
        "definitionParts":[{"path":"/Book.DataPipeline/part.txt","payload":"eA==","payloadType":"InlineBase64"}],
        "itemDefinitionsIndex":[{"id":PIPELINE,"rootPath":"/Book.DataPipeline"}],
    }));
    let http = host.http.clone();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &[
            "fab-rs",
            "bulk-export",
            "Demo.Workspace",
            "--recursive",
            "-f",
            "-o",
            "output",
        ],
        http,
    )
    .await;
    success(&output);
    assert_eq!(
        fs::read(selected.join("Book.DataPipeline").join("part.txt")).unwrap(),
        b"x"
    );
    assert!(
        matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
        if result.exported_ids == [PIPELINE])
    );
    assert_eq!(
        files
            .resolve_export_directory_async("output")
            .await
            .unwrap(),
        selected
    );
}
