// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Full deploy consumer with configuration-authorized, separately confined roots.

#![deny(unnameable_test_items)]

use super::{Harness, PIPELINE, WS, host, injected_export_with_http, success};
use fabric_cli::{
    deploy::DeployFailure,
    families::{FamilyEvidence, transfer_files::TokioDeployFiles},
};
use fabric_cli_transfer::TransferFiles;
use fabric_deploy::{
    definitions::DefinitionLimits, notebook::DefinitionLimits as NotebookDefinitionLimits,
};
use serde_json::json;
use std::{
    fs,
    future::{pending, ready},
    num::NonZeroUsize,
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

fn parameter_config(inputs: &Inputs, configuration: &Path) {
    fs::write(configuration, format!(
        "core:\n  workspace_id: {WS}\n  repository_directory: {}\n  item_types_in_scope: [CopyJob]\n  parameter: ../parameters/parameter.yml\nunpublish:\n  skip: true\nfeatures: [disable_workspace_folder_publish]\n",
        yaml_path(&inputs.0.join("repository")),
    )).unwrap();
}

fn metadata(host: &Harness) {
    host.reply(200, json!({"capacityId":"assigned"}));
    host.reply(200, json!({"value":[]}));
}

async fn loaded_files(configuration: &Path, limits: DefinitionLimits) -> TokioDeployFiles {
    let argument = configuration.to_str().unwrap().to_owned();
    let mut files = TokioDeployFiles::defer_config(
        configuration.to_owned(),
        argument,
        limits,
        tokio::runtime::Handle::current(),
    );
    let root = files
        .resolve_directory_async(configuration.parent().unwrap().to_str().unwrap())
        .await
        .unwrap();
    files
        .read_async(
            &root,
            configuration.file_name().unwrap().to_str().unwrap(),
            8192,
        )
        .await
        .unwrap();
    files
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

#[tokio::test]
async fn full_deploy_reads_parameter_and_template_roots_outside_the_configuration_directory() {
    let mut host = host();
    let inputs = Inputs::new(&host);
    let configuration = inputs.fixture();
    parameter_config(&inputs, &configuration);
    let parameters = inputs.0.join("parameters");
    let templates = inputs.0.join("templates");
    fs::create_dir(&parameters).unwrap();
    fs::create_dir(&templates).unwrap();
    fs::write(parameters.join("parameter.yml"),
        "extend: [../templates/shared.yml]\nfind_replace:\n  - find_value: original\n    replace_value: {test: changed}\n").unwrap();
    fs::write(
        templates.join("shared.yml"),
        "find_replace:\n  - find_value: changed\n    replace_value: {test: final}\n",
    )
    .unwrap();
    let argument = configuration.to_str().unwrap().to_owned();
    let mut files = TokioDeployFiles::defer_config(
        configuration,
        argument.clone(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    metadata(&host);
    host.reply(201, json!({"id":PIPELINE}));
    let http = host.http.clone();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &[
            "fab-rs",
            "deploy",
            "--config",
            &argument,
            "--target_env",
            "test",
            "-f",
        ],
        http,
    )
    .await;
    success(&output);
    let requests = host.requests();
    let part = requests[2]["body"]["definition"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["path"] == "content.json")
        .unwrap();
    let expected = serde_json::to_value(
        fabric_sdk::notebooks::DefinitionPart::inline("content.json", br#"{"value":"final"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(part["payload"], expected["payload"]);
}

#[tokio::test]
async fn full_deploy_missing_config_or_required_parameter_fails_before_network_mutation() {
    for parameter in [false, true] {
        let mut host = host();
        let inputs = Inputs::new(&host);
        let configuration = inputs.fixture();
        if parameter {
            parameter_config(&inputs, &configuration);
            fs::create_dir(inputs.0.join("parameters")).unwrap();
        } else {
            fs::remove_file(&configuration).unwrap();
        }
        let argument = configuration.to_str().unwrap().to_owned();
        let mut files = TokioDeployFiles::defer_config(
            configuration,
            argument.clone(),
            Default::default(),
            tokio::runtime::Handle::current(),
        );
        let http = host.http.clone();
        let output = injected_export_with_http(
            &mut host,
            &mut files,
            pending(),
            &["fab-rs", "deploy", "--config", &argument, "-f"],
            http,
        )
        .await;
        assert_ne!(output.plan.exit_code, 0);
        assert!(matches!(output.evidence, FamilyEvidence::Deploy(Err(_))));
        assert!(host.requests().is_empty());
    }
}

#[cfg(windows)]
#[tokio::test]
async fn full_deploy_exclusively_locked_configuration_retains_the_read_failure() {
    use std::os::windows::fs::OpenOptionsExt;
    let mut host = host();
    let inputs = Inputs::new(&host);
    let configuration = inputs.fixture();
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&configuration)
        .unwrap();
    let argument = configuration.to_str().unwrap().to_owned();
    let mut files = TokioDeployFiles::defer_config(
        configuration,
        argument.clone(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    let http = host.http.clone();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &["fab-rs", "deploy", "--config", &argument, "-f"],
        http,
    )
    .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(matches!(output.evidence, FamilyEvidence::Deploy(Err(_))));
    assert!(host.requests().is_empty());
    drop(locked);
}

#[tokio::test]
async fn deploy_reads_and_snapshots_keep_minimum_host_and_caller_limits() {
    for (host_bytes, caller_bytes) in [(8, 8192), (8192, 8)] {
        let host = host();
        let inputs = Inputs::new(&host);
        let configuration = inputs.fixture();
        let parent = configuration.parent().unwrap().to_owned();
        let limits = DefinitionLimits {
            max_bytes: NonZeroUsize::new(host_bytes).unwrap(),
            ..Default::default()
        };
        let mut files = TokioDeployFiles::defer_config(
            configuration.clone(),
            configuration.to_str().unwrap().into(),
            limits,
            tokio::runtime::Handle::current(),
        );
        let root = files
            .resolve_directory_async(parent.to_str().unwrap())
            .await
            .unwrap();
        assert!(
            files
                .read_async(&root, "deploy.yml", caller_bytes)
                .await
                .is_err()
        );
    }
    for (host_bytes, caller_bytes) in [(2048, 8192), (8192, 512)] {
        let host = host();
        let inputs = Inputs::new(&host);
        let configuration = inputs.fixture();
        fs::write(
            inputs
                .0
                .join("repository")
                .join("Example.CopyJob")
                .join("content.json"),
            vec![b'x'; 4096],
        )
        .unwrap();
        let mut files = loaded_files(
            &configuration,
            DefinitionLimits {
                max_bytes: NonZeroUsize::new(host_bytes).unwrap(),
                ..Default::default()
            },
        )
        .await;
        let root = files
            .resolve_directory_async(inputs.0.join("repository").to_str().unwrap())
            .await
            .unwrap();
        assert!(
            files
                .prepare_deployment_repository_async(
                    &root,
                    NotebookDefinitionLimits {
                        max_bytes: NonZeroUsize::new(caller_bytes).unwrap(),
                    },
                    100
                )
                .await
                .is_err()
        );
    }
    for (host_entries, caller_entries) in [(2, 100), (100, 2)] {
        let host = host();
        let inputs = Inputs::new(&host);
        let configuration = inputs.fixture();
        let mut files = loaded_files(
            &configuration,
            DefinitionLimits {
                max_files: NonZeroUsize::new(host_entries).unwrap(),
                ..Default::default()
            },
        )
        .await;
        let root = files
            .resolve_directory_async(inputs.0.join("repository").to_str().unwrap())
            .await
            .unwrap();
        assert!(
            files
                .prepare_deployment_repository_async(&root, Default::default(), caller_entries,)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn deploy_rejects_part_traversal_and_drive_wide_repository_authority() {
    let host = host();
    let inputs = Inputs::new(&host);
    let configuration = inputs.fixture();
    let mut files = loaded_files(&configuration, Default::default()).await;
    let root = files
        .resolve_directory_async(inputs.0.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        files
            .read_async(&root, "../configuration/deploy.yml", 8192)
            .await
            .is_err()
    );
    let drive = configuration.ancestors().last().unwrap().to_owned();
    let root = files
        .resolve_directory_async(drive.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        files
            .prepare_deployment_repository_async(&root, Default::default(), 100)
            .await
            .is_err()
    );
    assert!(host.requests().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn local_parent_selector_cannot_normalize_past_a_symlink() {
    let host = host();
    let inputs = Inputs::new(&host);
    let configuration = inputs.fixture();
    std::os::unix::fs::symlink(inputs.0.join("repository"), inputs.0.join("linked")).unwrap();
    let mut files = loaded_files(&configuration, Default::default()).await;
    let path = inputs.0.join("linked").join("..").join("repository");
    assert!(
        files
            .resolve_directory_async(path.to_str().unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn full_deploy_partial_publication_keeps_completed_identity_without_rollback() {
    let mut host = host();
    let inputs = Inputs::new(&host);
    let configuration = inputs.fixture();
    let second = inputs.0.join("repository").join("Second.CopyJob");
    fs::create_dir(&second).unwrap();
    fs::write(
        second.join(".platform"),
        serde_json::to_vec(&json!({
            "metadata":{"displayName":"Second","type":"CopyJob"},
            "config":{"logicalId":"55555555-5555-5555-5555-555555555555"},
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(second.join("content.json"), b"{}").unwrap();
    let argument = configuration.to_str().unwrap().to_owned();
    let mut files = TokioDeployFiles::defer_config(
        configuration,
        argument.clone(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    metadata(&host);
    host.reply(201, json!({"id":PIPELINE}));
    host.reply(403, json!({"errorCode":"Denied","message":"synthetic"}));
    let http = host.http.clone();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &["fab-rs", "deploy", "--config", &argument, "-f"],
        http,
    )
    .await;
    let FamilyEvidence::Deploy(Err(error)) = &output.evidence else {
        panic!("a real second-item service failure must not report success")
    };
    let DeployFailure::Workflow(error) = &error.failure else {
        panic!("partial workflow evidence must survive file capability composition")
    };
    assert_eq!(error.partial.published.len(), 1);
    assert_eq!(error.partial.published[0].outcome.item_id, PIPELINE);
    let requests = host.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        2
    );
    assert!(requests.iter().all(|request| request["method"] != "DELETE"));
}

#[tokio::test]
async fn full_deploy_cancellation_before_reads_never_activates_missing_config() {
    let mut host = host();
    let configuration = host.root.join("missing-directory").join("deploy.yml");
    let argument = configuration.to_str().unwrap().to_owned();
    let mut files = TokioDeployFiles::defer_config(
        configuration,
        argument.clone(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    let http = host.http.clone();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        ready(()),
        &["fab-rs", "deploy", "--config", &argument, "-f"],
        http,
    )
    .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(matches!(output.evidence, FamilyEvidence::Deploy(Err(_))));
    assert!(host.requests().is_empty());
}
