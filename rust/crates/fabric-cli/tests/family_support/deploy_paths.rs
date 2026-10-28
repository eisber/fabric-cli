// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Full deploy consumer with configuration-authorized, separately confined roots.

#![deny(unnameable_test_items)]

use super::{Harness, PIPELINE, WS, host, injected_export_with_http, success};
use fabric_cli::families::{FamilyEvidence, transfer_files::TokioDeployFiles};
use fabric_cli_transfer::TransferFiles;
use serde_json::json;
use std::{
    fs,
    future::pending,
    path::{Path, PathBuf},
};

struct Inputs(PathBuf);

impl Inputs {
    fn new(host: &Harness) -> Self {
        let path = host.root.with_file_name(format!(
            "{}-deploy-inputs",
            host.root.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn fixture(&self) -> PathBuf {
        let configuration = self.0.join("configuration");
        let repository = self.0.join("repository");
        fs::create_dir(&configuration).unwrap();
        let item = repository.join("Example.CopyJob");
        fs::create_dir_all(&item).unwrap();
        fs::write(
            item.join(".platform"),
            serde_json::to_vec(&json!({
                "metadata":{"displayName":"Example","type":"CopyJob"},
                "config":{"logicalId":"44444444-4444-4444-4444-444444444444"},
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(item.join("content.json"), br#"{"value":"original"}"#).unwrap();
        let path = configuration.join("deploy.yml");
        fs::write(&path, format!(
            "core:\n  workspace_id: {WS}\n  repository_directory: {}\n  item_types_in_scope: [CopyJob]\nunpublish:\n  skip: true\nfeatures: [disable_workspace_folder_publish]\n",
            yaml_path(&repository),
        )).unwrap();
        path
    }
}

impl Drop for Inputs {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn yaml_path(path: &Path) -> String {
    serde_json::to_string(path.to_str().unwrap()).unwrap()
}

#[tokio::test]
async fn full_deploy_reads_explicit_config_and_repository_outside_the_navigation_root() {
    let mut host = host();
    let inputs = Inputs::new(&host);
    let configuration = inputs.fixture();
    let argument = configuration.to_str().unwrap().to_owned();
    let mut files = TokioDeployFiles::defer_config(
        configuration,
        argument.clone(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    host.reply(200, json!({"capacityId":"assigned"}));
    host.reply(200, json!({"value":[]}));
    host.reply(201, json!({"id":PIPELINE}));
    let http = host.http.clone();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &["fab-rs", "deploy", "--config", &argument, "-f"],
        http,
    )
    .await;
    success(&output);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result))
        if result.completed.len() == 1)
    );
    assert_eq!(host.requests().len(), 3);
    assert_eq!(host.requests()[2]["body"]["type"], "CopyJob");
}

#[tokio::test]
async fn deploy_file_capability_pins_config_and_seals_after_repository_preparation() {
    let host = host();
    let inputs = Inputs::new(&host);
    let configuration = inputs.fixture();
    let argument = configuration.to_str().unwrap().to_owned();
    let parent = configuration.parent().unwrap().to_owned();
    let mut files = TokioDeployFiles::defer_config(
        configuration,
        argument,
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    assert!(
        files
            .resolve_directory_async(inputs.0.to_str().unwrap())
            .await
            .is_err()
    );
    let config_root = files
        .resolve_directory_async(parent.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        files
            .read_async(&config_root, "other.yml", 4096)
            .await
            .is_err()
    );
    let config_root = files
        .resolve_directory_async(parent.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        !files
            .read_async(&config_root, "deploy.yml", 4096)
            .await
            .unwrap()
            .is_empty()
    );
    let repository = inputs.0.join("repository");
    let repository = files
        .resolve_directory_async(repository.to_str().unwrap())
        .await
        .unwrap();
    let snapshot = files
        .prepare_deployment_repository_async(&repository, Default::default(), 100)
        .await
        .unwrap();
    assert_eq!(snapshot.items.len(), 1);
    assert!(
        files
            .resolve_directory_async(parent.to_str().unwrap())
            .await
            .is_err()
    );
    assert!(
        files
            .read_async(&config_root, "deploy.yml", 4096)
            .await
            .is_err()
    );
    assert!(host.requests().is_empty());
}
