// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public handler consumers; expected values are pinned Python source contracts.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use clap::Command;
use fabric_cli_local::{
    Capacity, ConfigStore, LocalCommand, LocalEffects, LocalError, LocalOutput,
    PREVIEW_VERSION, SETTINGS, SOURCE_PRODUCT_VERSION, execute, from_matches, root_commands,
};
use fabric_cli_output::Timestamp;
use fabric_cli_output::text::{OutputFormat, Presentation, ascii_width};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    cache: BTreeMap<String, String>,
    cache_calls: usize,
    fail_cache: bool,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "fabric-cli-local-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        Self { root, cache: BTreeMap::from([("workspace".to_owned(), "cached".to_owned())]), cache_calls: 0, fail_cache: false }
    }

    fn store(&self) -> ConfigStore {
        ConfigStore::new(self.root.join("fab"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

impl LocalEffects for Fixture {
    fn clear_cli_cache(&mut self) -> Result<(), LocalError> {
        self.cache_calls += 1;
        if self.fail_cache {
            return Err(LocalError::command("CacheError", "Injected cache failure"));
        }
        self.cache.clear();
        Ok(())
    }

    fn capacities(&mut self) -> Result<Vec<Capacity>, LocalError> {
        Ok(vec![Capacity { display_name: "Trial".to_owned(), id: "capacity-id".to_owned() }])
    }
}

fn parse(args: &[&str]) -> LocalCommand {
    let matches = Command::new("consumer").subcommands(root_commands())
        .try_get_matches_from(args).unwrap();
    from_matches(&matches).unwrap().unwrap().command
}

fn output(result: LocalOutput, command: &str, format: OutputFormat) -> (Vec<u8>, Vec<u8>) {
    let plan = result.render(
        command, Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), format,
        &ascii_width, Presentation::default(),
    ).unwrap();
    assert_eq!(plan.exit_code, 0);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    (stdout, stderr)
}

#[test]
fn config_set_and_get_really_persist_and_reload_source_strings() {
    let mut host = Fixture::new();
    let store = host.store();
    let set = parse(&["consumer", "config", "set", "DEBUG_ENABLED", " 'true' "]);
    let (stdout, stderr) = output(execute(&set, &store, &mut host).unwrap(), "config set", OutputFormat::Json);
    assert_eq!(stderr, b"Updating 'debug_enabled' value...\n");
    let envelope: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(envelope["result"]["message"], "Configuration 'debug_enabled' set to 'true'");
    let persisted = fs::read_to_string(store.root().join("config.json")).unwrap();
    assert!(persisted.starts_with("{\n    \"cache_enabled\": \"true\","));
    assert!(persisted.contains("\n    \"debug_enabled\": \"true\","));
    assert!(!persisted.ends_with('\n'));

    let reloaded = ConfigStore::new(store.root());
    let get = parse(&["consumer", "config", "get", "DEBUG_ENABLED"]);
    let (stdout, stderr) = output(execute(&get, &reloaded, &mut host).unwrap(), "config get", OutputFormat::Json);
    assert!(stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(envelope["result"]["data"], json!(["true"]));
}

#[test]
fn config_list_and_alias_use_source_order_all_keys_and_defaults() {
    let mut host = Fixture::new();
    let store = host.store();
    let command = parse(&["consumer", "config", "dir"]);
    assert_eq!(command, LocalCommand::ConfigList);
    let (stdout, stderr) = output(execute(&command, &store, &mut host).unwrap(), "config ls", OutputFormat::Json);
    assert!(stderr.is_empty());
    assert_eq!(
        fs::read_to_string(store.root().join("config.json")).unwrap(),
        include_str!("fixtures/defaults.json").trim_end_matches('\n'),
    );
    let envelope: Value = serde_json::from_slice(&stdout).unwrap();
    let rows = envelope["result"]["data"].as_array().unwrap();
    let expected = [
        ("cache_enabled", "true"), ("context_persistence_enabled", "false"),
        ("debug_enabled", "false"), ("encryption_fallback_enabled", "false"),
        ("job_cancel_ontimeout", "true"), ("local_definition_labels", ""),
        ("output_item_sort_criteria", "byname"), ("show_hidden", "false"),
        ("default_az_subscription_id", ""), ("default_az_resource_group", ""),
        ("default_az_location", ""), ("default_az_admin", ""), ("default_capacity", ""),
        ("default_open_experience", "fabric"), ("output_format", "text"),
        ("folder_listing_enabled", "false"), ("workspace_private_links_enabled", "false"),
        ("check_cli_version_updates", "true"),
    ];
    assert_eq!(rows.len(), 18);
    assert_eq!(SETTINGS.len(), 18);
    for (row, (key, value)) in rows.iter().zip(expected) {
        assert_eq!(row, &json!({"setting": key, "value": value}));
        assert_eq!(row.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), ["setting", "value"]);
    }
    let (text, _) = output(execute(&command, &store, &mut host).unwrap(), "config ls", OutputFormat::Text);
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("Setting"));
    assert!(text.contains("cache_enabled"));
}

#[test]
fn clear_cache_mutates_only_injected_cli_cache_and_propagates_failures() {
    let mut host = Fixture::new();
    let store = host.store();
    fs::create_dir_all(store.root()).unwrap();
    let credential_sentinel = store.root().join("cache.bin");
    fs::write(&credential_sentinel, b"untouched test sentinel, not a credential").unwrap();
    let command = parse(&["consumer", "config", "clear-cache"]);
    let (stdout, stderr) = output(execute(&command, &store, &mut host).unwrap(), "config clear-cache", OutputFormat::Json);
    assert!(host.cache.is_empty());
    assert_eq!(host.cache_calls, 1);
    assert!(stderr.is_empty());
    assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap()["result"]["message"], "Cleared the cache");
    assert_eq!(fs::read(&credential_sentinel).unwrap(), b"untouched test sentinel, not a credential");
    host.fail_cache = true;
    assert!(execute(&command, &store, &mut host).is_err());
}

#[test]
fn named_version_needs_no_config_and_distinguishes_preview_from_source() {
    let mut host = Fixture::new();
    let store = host.store();
    let command = parse(&["consumer", "version"]);
    let (stdout, stderr) = output(execute(&command, &store, &mut host).unwrap(), "version", OutputFormat::Json);
    assert_eq!(String::from_utf8(stdout).unwrap(), format!("fab version {PREVIEW_VERSION}\nhttps://aka.ms/fabric-cli/release-notes\n"));
    assert!(stderr.is_empty());
    assert!(!store.root().exists());
    assert_eq!(SOURCE_PRODUCT_VERSION, "1.8.0");
    assert_eq!(PREVIEW_VERSION, "0.1.0");
}

#[test]
fn validation_missing_get_mode_and_capacity_are_source_shaped() {
    let mut host = Fixture::new();
    let store = host.store();
    let missing_path = host.root.join("missing-label-file");
    let missing_path = missing_path.to_str().unwrap();
    for args in [
        vec!["consumer", "config", "set", "debug_enabled", "TRUE"],
        vec!["consumer", "config", "get", "unknown"],
        vec!["consumer", "config", "set", "local_definition_labels", missing_path],
    ] {
        let error = execute(&parse(&args), &store, &mut host).unwrap_err();
        let envelope = error.command_error().envelope(Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), None);
        assert!(envelope.result().error_code().is_some());
    }
    let missing = parse(&["consumer", "config", "get", "default_az_admin"]);
    assert_eq!(output(execute(&missing, &store, &mut host).unwrap(), "config get", OutputFormat::Json), (Vec::new(), Vec::new()));
    let capacity = parse(&["consumer", "config", "set", "default_capacity", "trial.Capacity"]);
    execute(&capacity, &store, &mut host).unwrap();
    let settings = store.read().unwrap();
    assert_eq!(settings["default_capacity"], "trial");
    assert_eq!(settings["fab_default_capacity_id"], "capacity-id");
    let mode = parse(&["consumer", "config", "get", "mode"]);
    let (stdout, stderr) = output(execute(&mode, &store, &mut host).unwrap(), "config get", OutputFormat::Json);
    assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap()["result"]["data"], json!(["command_line"]));
    assert!(String::from_utf8(stderr).unwrap().contains("'mode' setting is deprecated"));
}

#[test]
fn migration_preserves_unknown_settings_order_unicode_and_validates_json() {
    let mut host = Fixture::new();
    let store = host.store();
    fs::create_dir_all(store.root()).unwrap();
    fs::write(store.root().join("config.json"), r#"{"unrelated":"\u00e9","fab_debug_enabled":"true","mode":"cli"}"#).unwrap();
    execute(&LocalCommand::ConfigList, &store, &mut host).unwrap();
    let content = fs::read_to_string(store.root().join("config.json")).unwrap();
    assert!(content.starts_with("{\n    \"unrelated\": \"\\u00e9\","));
    assert!(!content.contains("\"mode\""));
    assert!(!content.contains("fab_debug_enabled"));
    assert_eq!(store.read().unwrap()["debug_enabled"], "true");
    fs::write(store.root().join("config.json"), b"{broken").unwrap();
    assert!(matches!(execute(&LocalCommand::ConfigList, &store, &mut host), Err(LocalError::InvalidConfig)));
    assert_eq!(fs::read(store.root().join("config.json")).unwrap(), b"{broken");
}

#[cfg(unix)]
#[test]
fn source_file_and_directory_modes_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mut host = Fixture::new();
    let store = host.store();
    execute(&LocalCommand::ConfigList, &store, &mut host).unwrap();
    assert_eq!(fs::metadata(store.root()).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(store.root().join("config.json")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn parser_rejects_bad_arity_and_shared_writer_propagates_failure() {
    assert!(Command::new("consumer").subcommands(root_commands()).try_get_matches_from(["consumer", "config", "set", "debug_enabled"]).is_err());
    let unrelated = Command::new("consumer").subcommand(Command::new("other")).try_get_matches_from(["consumer", "other"]).unwrap();
    assert_eq!(from_matches(&unrelated).unwrap(), None);
    let explicit = Command::new("consumer").subcommands(root_commands()).try_get_matches_from(["consumer", "config", "get", "debug_enabled", "--output_format", "json"]).unwrap();
    assert_eq!(from_matches(&explicit).unwrap().unwrap().output_format.as_deref(), Some("json"));
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::Error::other("writer failed")) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut host = Fixture::new();
    let result = execute(&LocalCommand::Version, &host.store(), &mut host).unwrap();
    let plan = result.render("version", Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), OutputFormat::Text, &ascii_width, Presentation::default()).unwrap();
    assert!(plan.write_to(&mut BrokenWriter, &mut Vec::new()).is_err());
}
