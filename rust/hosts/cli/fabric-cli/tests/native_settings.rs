// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native selection -> existing production read/job sessions.

#[allow(dead_code)]
mod folder_support;
mod job_execution_support;
#[path = "../src/native_settings.rs"]
mod settings;

use fabric_cli::{
    ItemSort,
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_jobs::JobConfig;
use fabric_cli_local::{ConfigStore, LocalError};
use fabric_cli_output::Timestamp;
use folder_support::{Credential, Http, Timer, complete, executor, reply, sdk};
use serde_json::{Map, Value, json};
use settings::{NativeSettings, SelectionError, command_deadline, command_watchdog};
use std::{
    future::pending,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn saved(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn job(args: &[&str]) -> fabric_cli_jobs::JobCommand {
    let root = fabric_cli::parser::command()
        .try_get_matches_from(std::iter::once("fab-rs").chain(args.iter().copied()))
        .unwrap();
    fabric_cli_jobs::command_from_matches(root.subcommand_matches("job").expect("job required"))
        .unwrap()
}

#[test]
fn omitted_store_retains_json_preview_and_selected_store_uses_source_defaults() {
    let preview = NativeSettings::select(None).unwrap();
    assert_eq!(preview.output(None).unwrap(), "json");
    assert!(!preview.reads.show_hidden);
    assert!(!preview.reads.folder_listing_enabled);
    assert!(preview.jobs.cancel_on_timeout);
    let selected = NativeSettings::select(Some(&Map::new())).unwrap();
    assert_eq!(selected.output(None).unwrap(), "text");
    assert_eq!(selected.reads.item_sort, ItemSort::ByName);
    assert!(selected.jobs.cancel_on_timeout);
}

#[test]
fn saved_settings_select_existing_fields_and_explicit_argv_wins() {
    let selected = NativeSettings::select(Some(&saved(json!({
        "show_hidden":"true",
        "folder_listing_enabled":"true",
        "output_item_sort_criteria":"bytype",
        "job_cancel_ontimeout":"false",
        "output_format":"text"
    }))))
    .unwrap();
    assert!(selected.reads.show_hidden);
    assert!(selected.reads.folder_listing_enabled);
    assert_eq!(selected.reads.item_sort, ItemSort::ByType);
    assert!(!selected.jobs.cancel_on_timeout);
    assert_eq!(selected.output(None).unwrap(), "text");
    assert_eq!(selected.output(Some("")).unwrap(), "text");
    assert_eq!(selected.output(Some("json")).unwrap(), "json");
    assert!(matches!(
        selected.output(Some("invalid")),
        Err(SelectionError::InvalidOutput)
    ));
    let command = job(&[
        "--output_format=json",
        "job",
        "run",
        "Book.Notebook",
        "--timeout=600",
        "--polling_interval=3",
    ]);
    let command = selected.job_command(&command).unwrap();
    assert_eq!(command.output_format.as_deref(), Some("json"));
    // Neither timeout nor interval has a saved setting in the pinned schema.
    assert_eq!(command.timeout, Some(600));
    assert_eq!(command.polling_interval, Some(3));
    assert!(
        command_watchdog(Some(&command), selected.jobs)
            .unwrap()
            .is_some()
    );
}

#[test]
fn saved_output_applies_to_every_job_leaf_and_read_flags_are_not_overwritten() {
    let selected = NativeSettings::select(Some(&saved(json!({"output_format":"text"})))).unwrap();
    for args in [
        vec!["job", "start", "Book.Notebook"],
        vec!["job", "run", "Book.Notebook"],
        vec!["job", "run-list", "Book.Notebook"],
        vec!["job", "run-status", "Book.Notebook", "--id=run"],
        vec!["job", "run-cancel", "Book.Notebook", "--id=run", "--wait"],
        vec!["job", "run-sch", "Book.Notebook", "--input={}"],
        vec![
            "job",
            "run-update",
            "Book.Notebook",
            "--id=schedule",
            "--input={}",
        ],
        vec!["job", "run-rm", "Book.Notebook", "--id=schedule", "--force"],
    ] {
        let command = job(&args);
        let resolved = selected.job_command(&command).unwrap();
        assert_eq!(resolved.output_format.as_deref(), Some("text"));
        assert_eq!(resolved.kind, command.kind);
        assert_eq!(resolved.wait, command.wait);
        assert_eq!(resolved.force, command.force);
        assert_eq!(resolved.id, command.id);
        assert_eq!(resolved.input, command.input);
    }
    let Invocation::Read(command) = parse_args(["fab-rs", "ls", "/", "--all"]).unwrap() else {
        panic!("read required")
    };
    let resolved = selected.read_command(&command).unwrap();
    assert!(resolved.all);
    assert_eq!(resolved.path, command.path);
    assert_eq!(resolved.output_format.as_deref(), Some("text"));
}

#[test]
fn consumed_saved_settings_reject_wrong_types_case_and_unknown_values_without_echo() {
    for (key, valid) in [
        ("show_hidden", "true"),
        ("folder_listing_enabled", "false"),
        ("output_item_sort_criteria", "bytype"),
        ("job_cancel_ontimeout", "true"),
        ("output_format", "json"),
    ] {
        for invalid in [
            Value::Null,
            json!(false),
            json!(1),
            json!([]),
            json!({}),
            json!(""),
            json!("private-invalid-value"),
            json!(valid.to_uppercase()),
        ] {
            let mut values = Map::new();
            values.insert(key.into(), invalid);
            let error = NativeSettings::select(Some(&values)).unwrap_err();
            assert!(matches!(error, SelectionError::InvalidSetting(found) if found == key));
            assert!(!error.to_string().contains("private-invalid-value"));
        }
    }
}

struct StoreFixture(PathBuf);

impl StoreFixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "native-settings-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
        )
    }
    fn store(&self) -> ConfigStore {
        ConfigStore::new(&self.0)
    }
}

impl Drop for StoreFixture {
    fn drop(&mut self) {
        if self.0.exists() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

#[test]
fn explicit_store_initialization_reuses_migration_and_does_not_replace_invalid_values() {
    let root = StoreFixture::new();
    let store = root.store();
    assert_eq!(
        NativeSettings::select(Some(&store.initialize().unwrap()))
            .unwrap()
            .output(None)
            .unwrap(),
        "text"
    );
    assert!(root.0.join("config.json").is_file());
    store
        .write(&saved(json!({
            "fab_show_hidden":"true", "fab_job_cancel_ontimeout":"false"
        })))
        .unwrap();
    let values = store.initialize().unwrap();
    let selected = NativeSettings::select(Some(&values)).unwrap();
    assert!(selected.reads.show_hidden);
    assert!(!selected.jobs.cancel_on_timeout);
    assert_eq!(selected.output(None).unwrap(), "text");
    assert!(!values.contains_key("fab_show_hidden"));
    store
        .write(&saved(json!({"job_cancel_ontimeout":false})))
        .unwrap();
    assert!(matches!(
        NativeSettings::select(Some(&store.initialize().unwrap())),
        Err(SelectionError::InvalidSetting("job_cancel_ontimeout"))
    ));
    assert_eq!(store.read().unwrap()["job_cancel_ontimeout"], false);
    std::fs::write(root.0.join("config.json"), "{").unwrap();
    assert!(matches!(store.initialize(), Err(LocalError::InvalidConfig)));
}

#[test]
fn saved_hidden_names_reach_real_read_session_and_explicit_output_still_wins() {
    let selected = NativeSettings::select(Some(&saved(json!({
        "show_hidden":"true", "output_format":"text"
    }))))
    .unwrap();
    let Invocation::Read(command) =
        parse_args(["fab-rs", "--output_format=json", "ls", "/"]).unwrap()
    else {
        panic!("read required")
    };
    let command = selected.read_command(&command).unwrap();
    let http = Arc::new(Http::new([reply(json!({"value":[]}))]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::new(Credential::default()));
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: selected.reads,
    };
    let plan = complete(session.dispatch(&command, pending())).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    plan.write_to(&mut out, &mut err).unwrap();
    assert_eq!(plan.exit_code, 0);
    assert!(err.is_empty());
    let envelope: Value = serde_json::from_slice(&out).unwrap();
    assert!(
        envelope["result"]["hidden_data"]
            .as_array()
            .unwrap()
            .contains(&json!(".capacities"))
    );
    http.assert_consumed();
}

fn job_case(selected: NativeSettings, extra: &[&str], replies: Vec<Value>) -> Value {
    let mut args = vec!["job", "run", "Demo.Workspace/Book.Notebook"];
    args.extend_from_slice(extra);
    let command = selected.job_command(&job(&args)).unwrap();
    // The existing probe invokes the production JobSession with synthetic
    // credential/HTTP/time and the selected family arguments/configuration.
    args.insert(0, command.output_format.as_deref().unwrap());
    args.insert(0, "--output_format");
    let mut responses = job_execution_support::metadata("Notebook");
    responses.extend(replies);
    json!({
        "command":args, "responses":responses,
        "cancel_on_timeout":selected.jobs.cancel_on_timeout
    })
}

#[test]
fn saved_cancel_policy_controls_actual_timeout_post_without_claiming_completion() {
    for cancel in [false, true] {
        let selected = NativeSettings::select(Some(&saved(json!({
            "job_cancel_ontimeout": if cancel { "true" } else { "false" },
            "output_format":"json"
        }))))
        .unwrap();
        let mut replies = vec![job_execution_support::accepted()];
        if cancel {
            replies.push(json!({"status":202,"body":null}));
        }
        let observation =
            job_execution_support::observe(&job_case(selected, &["--timeout=1"], replies));
        assert_eq!(observation["exit"], 0);
        assert_eq!(observation["waits"], json!([1]));
        assert_eq!(
            observation["requests"].as_array().unwrap().len(),
            if cancel { 4 } else { 3 }
        );
        assert!(
            !observation["stdout"]
                .as_str()
                .unwrap()
                .contains("completed")
        );
        if cancel {
            assert!(
                observation["requests"][3]["url"]
                    .as_str()
                    .unwrap()
                    .ends_with("/run/cancel")
            );
            assert!(
                observation["stdout"]
                    .as_str()
                    .unwrap()
                    .contains("cancelled (async)")
            );
        } else {
            assert_eq!(observation["stdout"], "");
            assert!(
                observation["stderr"]
                    .as_str()
                    .unwrap()
                    .contains("Job still running")
            );
        }
    }
}

#[test]
fn saved_job_configuration_preserves_success_service_errors_and_local_cancellation() {
    let selected = NativeSettings::select(Some(&saved(json!({
        "job_cancel_ontimeout":"false", "output_format":"json"
    }))))
    .unwrap();
    let success = job_execution_support::observe(&job_case(
        selected,
        &["--timeout=600", "--polling_interval=2"],
        vec![
            job_execution_support::accepted(),
            json!({"status":200,"body":{"status":"Completed"}}),
        ],
    ));
    assert_eq!(success["exit"], 0);
    assert_eq!(success["waits"], json!([2]));
    assert!(success["stdout"].as_str().unwrap().contains("completed"));
    let failure = job_execution_support::observe(&job_case(
        selected,
        &["--timeout=600"],
        vec![json!({"status":401,"body":{"errorCode":"Unauthorized","message":"synthetic"}})],
    ));
    assert_eq!(failure["exit"], 4);
    assert_eq!(failure["requests"].as_array().unwrap().len(), 3);
    let mut cancelled = job_case(
        selected,
        &["--timeout=600"],
        vec![job_execution_support::accepted()],
    );
    cancelled["hold_after"] = json!(4);
    let cancelled = job_execution_support::observe(&cancelled);
    assert_eq!(cancelled["exit"], 1);
    assert!(
        cancelled["stdout"]
            .as_str()
            .unwrap()
            .contains("LongRunningOperationCancelled")
    );
    assert_eq!(cancelled["requests"].as_array().unwrap().len(), 4);
    assert_eq!(cancelled["requests"][3]["method"], "GET");
}

#[test]
fn only_explicit_nonnegative_synchronous_timeout_extends_the_preview_watchdog() {
    for args in [
        vec!["job", "run", "Book.Notebook"],
        vec!["job", "run", "Book.Notebook", "--timeout=-1"],
        vec!["job", "start", "Book.Notebook"],
        vec!["job", "run-status", "Book.Notebook", "--id=run"],
        vec!["job", "run-cancel", "Book.Notebook", "--id=run", "--wait"],
    ] {
        assert_eq!(
            command_deadline(Some(&job(&args)), JobConfig::default()).unwrap(),
            Duration::from_secs(240)
        );
    }
    assert_eq!(
        command_deadline(None, JobConfig::default()).unwrap(),
        Duration::from_secs(240)
    );
}

#[test]
fn explicit_job_deadline_includes_elapsed_rounding_overshoot_and_optional_cleanup() {
    for timeout in [0_u64, 1, 240, 600, 86_400] {
        let seconds = format!("--timeout={timeout}");
        let command = job(&["job", "run", "Book.Notebook", &seconds]);
        let leave_running = command_deadline(
            Some(&command),
            JobConfig {
                cancel_on_timeout: false,
            },
        )
        .unwrap();
        // Setup + integer-accounted job budget + final HTTP exchange + poll overshoot.
        assert_eq!(
            leave_running,
            Duration::from_secs(240 + timeout * 2 + 240 + 60)
        );
        let cancel = command_deadline(
            Some(&command),
            JobConfig {
                cancel_on_timeout: true,
            },
        )
        .unwrap();
        // Cancellation POST, then the existing 100-request bounded reconciler.
        assert_eq!(
            cancel - leave_running,
            Duration::from_secs(240 + 100 * (240 + 60))
        );
        assert!(leave_running > Duration::from_secs(timeout));
    }
    let overflow = job(&[
        "job",
        "run",
        "Book.Notebook",
        "--timeout=9223372036854775807",
    ]);
    assert!(matches!(
        command_deadline(Some(&overflow), JobConfig::default()),
        Err(SelectionError::DeadlineRange)
    ));
}
