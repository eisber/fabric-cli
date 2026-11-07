// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

//! Caller-owned command preflight and cancellation tests.

use clap::Command;
use fabric_cli_semantic_model::{AnalysisFiles, AnalyzeCommand, CommandError, command, execute};
use fabric_sdk_semantic_model::{AnalysisBackend, AnalysisError};
use std::{path::Path, sync::atomic::AtomicBool};

fn parse(arguments: &[&str]) -> AnalyzeCommand {
    let values = Command::new("fab-cli")
        .subcommand(command())
        .try_get_matches_from(arguments)
        .unwrap();
    AnalyzeCommand::from_matches(&values).unwrap()
}

struct NoFiles;
impl AnalysisFiles for NoFiles {
    fn read(&mut self, _: &Path, _: usize) -> Result<String, CommandError> {
        panic!("preflight must precede all file access");
    }
}

struct NoBackend;
impl AnalysisBackend for NoBackend {
    fn analyze(&self, _: &[u8], _: usize) -> Result<Vec<u8>, AnalysisError> {
        panic!("preflight must precede native activation");
    }
}

#[test]
fn mutation_is_not_analysis_and_never_opens_input() {
    let parsed = parse(&[
        "fab-cli",
        "artifact",
        "semantic-model",
        "analyze",
        "--input",
        "model.bim",
        "--fix",
    ]);
    assert!(matches!(
        execute(&parsed, &mut NoFiles, &NoBackend, &AtomicBool::new(false),),
        Err(CommandError::Unsupported(_))
    ));
}

#[test]
fn remote_path_does_not_fall_back_to_local_or_another_transport() {
    let parsed = parse(&[
        "fab-cli",
        "artifact",
        "semantic-model",
        "analyze",
        "/demo.Workspace/model.SemanticModel",
    ]);
    assert!(matches!(
        execute(&parsed, &mut NoFiles, &NoBackend, &AtomicBool::new(false),),
        Err(CommandError::Unsupported(_))
    ));
}

#[test]
fn local_input_and_remote_path_are_distinct_authorities() {
    assert!(
        Command::new("fab-cli")
            .subcommand(command())
            .try_get_matches_from([
                "fab-cli",
                "artifact",
                "semantic-model",
                "analyze",
                "/model",
                "--input",
                "model.bim",
            ])
            .is_err()
    );
}

#[test]
fn cancellation_is_observed_before_reading_input() {
    let parsed = parse(&[
        "fab-cli",
        "artifact",
        "semantic-model",
        "analyze",
        "--input",
        "model.bim",
    ]);
    assert!(matches!(
        execute(&parsed, &mut NoFiles, &NoBackend, &AtomicBool::new(true),),
        Err(CommandError::Analysis(AnalysisError::Cancelled))
    ));
}
