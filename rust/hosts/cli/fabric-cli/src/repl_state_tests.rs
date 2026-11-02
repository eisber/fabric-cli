// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive contracts for the recovered state/control-flow seams.

#![deny(unnameable_test_items)]

use super::*;

#[test]
fn returned_failures_are_not_terminal_and_cancellation_does_not_depend_on_exit() {
    for exit in [0, 1, 2, 4] {
        assert_eq!(
            CommandOutcome::from_code(Some("ReadFailed"), exit),
            CommandOutcome::Returned(exit)
        );
        assert_eq!(
            CommandOutcome::from_code(Some("OperationCancelled"), exit),
            CommandOutcome::Cancelled(exit)
        );
        assert_eq!(
            CommandOutcome::from_code(Some("UnexpectedError"), exit),
            CommandOutcome::Stop(exit)
        );
    }
}

#[test]
fn child_exit_conversion_preserves_every_status_without_inventing_cancellation() {
    for exit in 0..=u8::MAX {
        assert_eq!(
            CommandOutcome::from_exit(ExitCode::from(exit)),
            CommandOutcome::Returned(exit)
        );
    }
}

#[test]
fn interactive_navigation_never_restores_or_overwrites_a_command_line_context_file() {
    use fabric_cli_context::{Catalog, HierarchyCatalog};
    use fabric_cli_local::ConfigStore;
    use std::{fs, path::PathBuf};

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("target")
        .join(format!("repl-state-contract-{}", std::process::id()));
    assert!(
        !root.exists(),
        "test-owned root must not replace existing data"
    );
    let store = ConfigStore::new(root.clone());
    let mut settings = store.initialize().unwrap();
    settings.insert("context_persistence_enabled".into(), "true".into());
    store.write(&settings).unwrap();
    let mut state = ReplState::new_interactive(Ok(("/".into(), false)));
    state.initialize_local().unwrap();
    let navigation = state.navigation.as_mut().unwrap();
    let file = navigation.context_file(&store);
    let original = b"synthetic corrupt context must not be read";
    fs::write(&file, original).unwrap();
    let lookup = HierarchyCatalog::new(Catalog::new(Vec::new()), Vec::new(), Vec::new());
    assert!(!navigation.restore(&store, &lookup).unwrap());
    assert!(!navigation.is_command_line());
    navigation
        .change(&store, Arc::clone(navigation.current()), true)
        .unwrap();
    assert_eq!(fs::read(&file).unwrap(), original);
    assert_eq!(state.path().unwrap(), "/");
    fs::remove_dir_all(&root).unwrap();
}

#[cfg(all(feature = "native-read", windows))]
#[test]
fn native_binding_replaces_only_the_local_root_and_retains_interactive_runtime_mode() {
    let mut state = ReplState::new_interactive(Ok(("/Demo.Workspace".into(), false)));
    state.initialize_local().unwrap();
    state
        .bind("tenant", "account", "client", HomeContext::Unspecified)
        .unwrap();
    assert!(!state.navigation.as_ref().unwrap().is_command_line());
    assert_eq!(
        state
            .navigation
            .as_ref()
            .unwrap()
            .current()
            .root()
            .unwrap()
            .id(),
        Some("tenant")
    );
    let before = Arc::clone(state.navigation.as_ref().unwrap().current());
    state
        .bind("tenant", "account", "client", HomeContext::Unspecified)
        .unwrap();
    assert!(Arc::ptr_eq(
        &before,
        state.navigation.as_ref().unwrap().current()
    ));
    assert!(
        state
            .bind("other", "account", "client", HomeContext::Unspecified)
            .is_err()
    );
    assert_eq!(state.path().unwrap(), "/Demo.Workspace");
}
