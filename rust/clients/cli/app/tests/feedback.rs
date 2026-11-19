// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public parser, manifest and executable feedback contracts; all I/O is offline.
#![deny(unnameable_test_items)]

use fabric_cli::{
    feedback::{Configuration, Environment, FeedbackCommand},
    parser::{Invocation, parse_args},
};
use serde_json::Value;
use std::process::{Command, Output};

const ID: &str = "11111111-2222-4333-8444-555555555555";

fn input(send: bool) -> FeedbackCommand {
    FeedbackCommand {
        title: "Caller title".into(),
        message: "Caller feedback\nsecond line".into(),
        feedback_id: Some(ID.into()),
        send,
        output_format: None,
    }
}

fn config(environment: Environment) -> Configuration {
    Configuration {
        environment,
        feature_area: None,
    }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    "2026-11-16T10:38:58.123Z".parse().unwrap()
}

fn executable(args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.args(args);
    for name in [
        "FAB_RUST_FEEDBACK_ENVIRONMENT",
        "FAB_RUST_FEEDBACK_APP_ID",
        "FAB_RUST_FEEDBACK_FEATURE_AREA",
        "FAB_RUST_AUTH_MODE",
        "FAB_RUST_CONFIG_ROOT",
    ] {
        command.env_remove(name);
    }
    // Any accidental navigation/configuration access fails instead of using a
    // developer's real persisted selection.
    command.env("FAB_RUST_CONFIG_ROOT", "invalid-relative-feedback-test");
    command.envs(vars.iter().copied());
    command.output().unwrap()
}

#[test]
fn root_and_alias_parse_without_ambient_configuration() {
    for name in ["feedback", "report"] {
        let Invocation::Feedback(command) = parse_args([
            "fab-rs",
            name,
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            ID,
        ])
        .unwrap() else {
            panic!("feedback dispatch missing")
        };
        assert!(!command.send);
        assert_eq!(command.feedback_id.as_deref(), Some(ID));
    }
    let Invocation::Feedback(command) = parse_args([
        "fab-rs",
        "--output_format",
        "text",
        "report",
        "--send",
        "--title",
        "t",
        "--message",
        "m",
        "--feedback-id",
        ID,
    ])
    .unwrap() else {
        panic!("feedback dispatch missing")
    };
    assert!(command.send);
    assert_eq!(command.output_format.as_deref(), Some("text"));
}

#[test]
fn omitted_id_remains_unresolved_during_pure_parsing() {
    let Invocation::Feedback(command) =
        parse_args(["fab-rs", "feedback", "--title", "t", "--message", "m"]).unwrap()
    else {
        panic!("feedback dispatch missing")
    };
    assert!(command.feedback_id.is_none());
    assert!(command.validate().is_ok());
}

#[test]
fn executable_generates_a_new_v4_id_with_built_in_profiles() {
    let mut ids = std::collections::BTreeSet::new();
    for (name, environment) in [("feedback", "ppe"), ("report", "prod"), ("feedback", "ppe")] {
        let output = executable(
            &[name, "--title", "t", "--message", "m"],
            &[("FAB_RUST_FEEDBACK_ENVIRONMENT", environment)],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let output: Value = serde_json::from_slice(&output.stdout).unwrap();
        let id = output["client_feedback_id"].as_str().unwrap();
        let uuid = uuid::Uuid::parse_str(id).unwrap();
        assert_eq!(uuid.get_version_num(), 4);
        assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
        assert_eq!(output["payload"]["clientFeedbackId"], id);
        assert_eq!(output["environment"], environment);
        assert_eq!(output["app_id"], 2931);
        assert_eq!(output["payload"]["appId"], 2931);
        assert_eq!(output["delivery"], "not_sent");
        assert!(ids.insert(id.to_owned()));
    }
}

#[test]
fn environment_and_routing_are_strict_and_send_requires_explicit_environment() {
    assert_eq!(
        Configuration::capture(false, |_| Ok(None))
            .unwrap()
            .environment,
        Environment::Production
    );
    assert!(Configuration::capture(true, |_| Ok(None)).is_err());
    for environment in ["", "PPE", "https://evil.invalid", "production", "ppe "] {
        assert!(
            Configuration::capture(true, |name| Ok(
                (name == "FAB_RUST_FEEDBACK_ENVIRONMENT").then(|| environment.into())
            ))
            .is_err()
        );
    }
    let captured = Configuration::capture(true, |name| {
        Ok(match name {
            "FAB_RUST_FEEDBACK_ENVIRONMENT" => Some("ppe".into()),
            "FAB_RUST_FEEDBACK_APP_ID" => None,
            "FAB_RUST_FEEDBACK_FEATURE_AREA" => Some("Owner supplied area".into()),
            _ => panic!("unexpected environment read"),
        })
    })
    .unwrap();
    assert_eq!(captured.environment, Environment::Ppe);
    assert_eq!(captured.environment.app_id(), 2931);
    assert_eq!(Environment::Production.app_id(), 2931);
    let legacy = Configuration::capture(false, |name| {
        Ok((name == "FAB_RUST_FEEDBACK_APP_ID").then(|| "2931".into()))
    })
    .unwrap();
    assert_eq!(legacy.environment.app_id(), 2931);
    assert_eq!(
        captured.feature_area.as_deref(),
        Some("Owner supplied area")
    );
    assert!(Configuration::capture(false, |_| Err("invalid Unicode")).is_err());
    for area in ["", "  ", "routing\r\ninjection"] {
        assert!(
            Configuration::capture(false, |name| Ok(
                (name == "FAB_RUST_FEEDBACK_FEATURE_AREA").then(|| area.into())
            ))
            .is_err()
        );
    }
}

#[test]
fn input_bounds_use_utf8_bytes_and_reject_bad_ids_without_echo() {
    let mut command = input(false);
    command.title = "x".repeat(256);
    command.message = "x".repeat(16384);
    assert!(command.validate().is_ok());
    command.title.push('x');
    assert!(command.validate().is_err());
    command.title = "\u{e9}".repeat(129);
    assert!(command.validate().is_err());
    command.title = "title\ninjected".into();
    assert!(command.validate().is_err());
    command.title = "valid".into();
    command.message.push('x');
    assert!(command.validate().is_err());
    for id in [
        "private-invalid-value",
        "",
        "00000000-0000-0000-0000-000000000000",
        "11111111/2222-4333-8444-555555555555",
    ] {
        command = input(false);
        command.feedback_id = Some(id.into());
        let error = command.validate().unwrap_err();
        assert!(!error.contains("private-invalid-value"));
    }
    command = input(false);
    command.message = "\u{1b}[31m".into();
    assert!(command.validate().is_err());
}

#[test]
fn manifest_matches_verified_petrol_wire_contract_without_fabricated_telemetry() {
    let manifest = input(false)
        .manifest(&config(Environment::Ppe), now())
        .unwrap();
    assert_eq!(manifest["appId"], 2931);
    assert_eq!(manifest["clientFeedbackId"], ID);
    assert_eq!(manifest["manifestType"], "Sas");
    assert_eq!(manifest["source"], "Client");
    assert_eq!(manifest["submissionType"], "FormSubmit");
    assert_eq!(manifest["submitTime"], "2026-11-16T10:38:58.123Z");
    assert_eq!(manifest["comment"], "Caller feedback\nsecond line");
    assert_eq!(
        manifest["responseContent"]["elements"][0]["userVerbatim"],
        manifest["comment"]
    );
    assert_eq!(manifest["telemetry"]["audience"], "PPE");
    assert!(manifest["telemetry"].get("application").is_none());
    assert!(manifest["telemetry"].get("featureArea").is_none());
    assert_eq!(
        manifest["complianceChecks"],
        serde_json::json!({"authenticationType":"AAD"})
    );
    assert_eq!(
        manifest["userConsentTypes"]["emailConsentType"],
        "Not Requested"
    );
    assert_eq!(
        manifest["userConsentTypes"]["contentSamplesConsentType"],
        "None"
    );
    assert!(manifest.get("email").is_none());
    assert!(manifest.get("attachments").is_none());
    assert_eq!(manifest["shouldPromoteToADO"], false);
    let app: Value =
        serde_json::from_str(manifest["application"]["appData"].as_str().unwrap()).unwrap();
    assert_eq!(app["client"], "fab-rs");
    assert_eq!(app["environment"], "ppe");
    let mut routing = config(Environment::Production);
    routing.feature_area = Some("Explicit area".into());
    let manifest = input(false).manifest(&routing, now()).unwrap();
    assert_eq!(manifest["telemetry"]["featureArea"], "Explicit area");
    assert_eq!(manifest["telemetry"]["audience"], "Production");
}

#[test]
fn encoded_manifest_cap_and_public_configuration_validation_precede_authentication() {
    let mut command = input(false);
    command.message = "\"".repeat(16384);
    assert!(command.validate().is_ok());
    assert!(command.manifest(&config(Environment::Ppe), now()).is_err());
    let mut invalid = config(Environment::Ppe);
    invalid.feature_area = Some("invalid\narea".into());
    assert!(input(false).manifest(&invalid, now()).is_err());
    let mut unresolved = input(false);
    unresolved.feedback_id = None;
    assert!(unresolved.validate().is_ok());
    assert!(
        unresolved
            .manifest(&config(Environment::Ppe), now())
            .is_err()
    );
}

#[test]
fn executable_preview_and_help_do_not_touch_configuration_or_identity() {
    for name in ["feedback", "report"] {
        let result = executable(
            &[name, "--title", "t", "--message", "m", "--feedback-id", ID],
            &[
                ("FAB_RUST_FEEDBACK_ENVIRONMENT", "ppe"),
                ("FAB_RUST_AUTH_MODE", "deliberately-unsupported"),
            ],
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty());
        let output: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(output["delivery"], "not_sent");
        assert_eq!(output["endpoint"], Environment::Ppe.endpoint());
        assert_eq!(output["payload"]["appId"], 2931);
        let result = executable(
            &[name, "--help"],
            &[("FAB_RUST_FEEDBACK_ENVIRONMENT", "invalid")],
        );
        assert!(result.status.success());
        assert!(String::from_utf8(result.stdout).unwrap().contains("--send"));
    }
}

#[test]
fn executable_invalid_input_and_environment_are_sanitized_and_never_sent() {
    let obsolete = executable(
        &["feedback", "--title", "t", "--message", "m"],
        &[
            ("FAB_RUST_FEEDBACK_ENVIRONMENT", "ppe"),
            ("FAB_RUST_FEEDBACK_APP_ID", "private-obsolete-app"),
        ],
    );
    assert_eq!(obsolete.status.code(), Some(2));
    assert!(obsolete.stdout.is_empty());
    let error = String::from_utf8(obsolete.stderr).unwrap();
    assert!(error.contains("FAB_RUST_FEEDBACK_APP_ID"));
    assert!(!error.contains("private-obsolete-app"));
    let result = executable(
        &[
            "feedback",
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            ID,
            "--send",
        ],
        &[],
    );
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("FAB_RUST_FEEDBACK_ENVIRONMENT")
    );
    let result = executable(
        &[
            "feedback",
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            "private-invalid-value",
        ],
        &[],
    );
    assert_eq!(result.status.code(), Some(2));
    assert!(
        !String::from_utf8(result.stderr)
            .unwrap()
            .contains("private-invalid-value")
    );
}

#[test]
fn executable_text_preview_preserves_content_and_correlation() {
    let result = executable(
        &[
            "--output_format",
            "text",
            "feedback",
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            ID,
        ],
        &[],
    );
    assert!(result.status.success());
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("not_sent: environment=prod"));
    assert!(output.contains(ID));
    assert!(output.contains("\"feedbackTitle\": \"t\""));
}

#[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
#[test]
fn native_send_rejects_other_auth_providers_before_selection_or_network_access() {
    let result = executable(
        &[
            "feedback",
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            ID,
            "--send",
        ],
        &[
            ("FAB_RUST_FEEDBACK_ENVIRONMENT", "ppe"),
            ("FAB_RUST_AUTH_MODE", "device-code"),
        ],
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let output: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(output["delivery"], "not_sent");
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("not silently replaced")
    );
}

#[test]
fn mixed_batch_and_feedback_arguments_cannot_silently_discard_submission_intent() {
    assert!(
        parse_args([
            "fab-rs",
            "-c",
            "version",
            "feedback",
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            ID,
        ])
        .is_err()
    );
    assert!(matches!(
        fabric_cli::parser::batch::parse_command(&[
            "feedback",
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            ID,
        ])
        .unwrap()
        .0,
        Invocation::Feedback(_)
    ));
}

#[cfg(not(all(windows, feature = "native-host", feature = "windows-broker")))]
#[test]
fn sending_without_native_capability_fails_without_success_output() {
    let result = executable(
        &[
            "feedback",
            "--title",
            "t",
            "--message",
            "m",
            "--feedback-id",
            ID,
            "--send",
        ],
        &[("FAB_RUST_FEEDBACK_ENVIRONMENT", "ppe")],
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let output: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(output["delivery"], "not_sent");
    assert_eq!(output["client_feedback_id"], ID);
    assert!(output["message"].as_str().unwrap().contains("native-read"));
}

#[cfg(feature = "native-host")]
#[path = "feedback_support/wire.rs"]
mod wire;
