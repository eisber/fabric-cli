// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public consumers for six local leaves; no native browser or terminal is used.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clap::Command;
use fabric_cli_context::{
    Catalog, Element, HierarchyCatalog, HomeContext, ItemType, NodeKind, PathElement, WorkspaceType,
};
use fabric_cli_local::{
    AnsiTerminal, BrowserIdentity, BrowserLauncher, Capacity, ConfigStore, LocalCommand,
    LocalEffects, LocalError, LocalOutput, NavigationHost, NavigationState, SupportMatrix,
    TerminalControl, execute, from_matches, root_commands,
};
use fabric_cli_output::Timestamp;
use fabric_cli_output::text::{OutputFormat, Presentation, ascii_width};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    tenant: Arc<PathElement>,
    catalog: HierarchyCatalog,
    browser: CaptureBrowser,
    terminal: AnsiTerminal<Vec<u8>>,
    user: bool,
    browser_failure: bool,
}

#[derive(Default)]
struct CaptureBrowser(Vec<String>);

impl BrowserLauncher for CaptureBrowser {
    fn open_new(&mut self, url: &str) -> Result<(), LocalError> {
        self.0.push(url.to_owned());
        Ok(())
    }
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "fabric-cli-navigation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        let tenant = Element::tenant("Synthetic", "tenant-id");
        let workspace = Element::child(
            "W",
            Some("workspace-id".into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            Arc::clone(&tenant),
        )
        .unwrap();
        let personal = Element::child(
            "Home",
            Some("home-id".into()),
            NodeKind::Workspace(WorkspaceType::Personal),
            Arc::clone(&tenant),
        )
        .unwrap();
        let note = Element::child(
            "Note",
            Some("note-id".into()),
            NodeKind::Item(ItemType::parse("Notebook").unwrap()),
            Arc::clone(&workspace),
        )
        .unwrap();
        let catalog = HierarchyCatalog::new(
            Catalog::new(vec![workspace, personal, note]),
            Vec::new(),
            Vec::new(),
        );
        Self {
            root,
            tenant: PathElement::from_fabric(tenant),
            catalog,
            browser: CaptureBrowser::default(),
            terminal: AnsiTerminal::new(Vec::new()),
            user: true,
            browser_failure: false,
        }
    }

    fn store(&self) -> ConfigStore {
        ConfigStore::new(self.root.join("fab"))
    }

    fn state(&self, command_line: bool) -> NavigationState {
        NavigationState::new(
            Arc::clone(&self.tenant),
            HomeContext::Workspace("Home.Personal".into()),
            self.root.to_str().unwrap().to_owned(),
            4242,
            command_line,
        )
        .unwrap()
    }

    fn run(
        &mut self,
        state: &mut NavigationState,
        command: &LocalCommand,
    ) -> Result<LocalOutput, LocalError> {
        let store = self.store();
        // The metadata graph is immutable and cloneable; no service or global host.
        let catalog = self.catalog.clone();
        let mut host = NavigationHost {
            state,
            lookup: &catalog,
            store: &store,
            effects: self,
        };
        execute(command, &store, &mut host)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

impl LocalEffects for Fixture {
    fn clear_cli_cache(&mut self) -> Result<(), LocalError> {
        Ok(())
    }
    fn capacities(&mut self) -> Result<Vec<Capacity>, LocalError> {
        Ok(Vec::new())
    }
    fn browser_identity(&self) -> Result<BrowserIdentity, LocalError> {
        Ok(BrowserIdentity {
            user: self.user,
            claims: vec![
                ("tid".into(), "tenant-id".into()),
                ("upn".into(), "synthetic@example.invalid".into()),
            ],
        })
    }
    fn open_browser(&mut self, url: &str) -> Result<(), LocalError> {
        if self.browser_failure {
            return Err(LocalError::command(
                "BrowserLaunchFailed",
                "Injected failure",
            ));
        }
        self.browser.open_new(url)
    }
    fn clear_terminal(&mut self) -> Result<(), LocalError> {
        self.terminal.clear()
    }
}

fn parse(args: &[&str]) -> LocalCommand {
    let matches = Command::new("consumer")
        .subcommands(root_commands())
        .try_get_matches_from(args)
        .unwrap();
    from_matches(&matches).unwrap().unwrap().command
}

fn output(result: LocalOutput, command: &str) -> (Vec<u8>, Vec<u8>, u8) {
    let plan = result
        .render(
            command,
            Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            OutputFormat::Json,
            &ascii_width,
            Presentation::default(),
        )
        .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    (stdout, stderr, plan.exit_code)
}

#[test]
fn cd_resolves_changes_and_persists_then_another_state_restores() {
    let mut fixture = Fixture::new();
    let store = fixture.store();
    let mut state = fixture.state(true);
    execute(
        &LocalCommand::ConfigSet {
            key: "context_persistence_enabled".into(),
            value: "true".into(),
        },
        &store,
        &mut fixture,
    )
    .unwrap();
    let command = parse(&["consumer", "cd", "/W.Workspace"]);
    let (stdout, stderr, code) = output(fixture.run(&mut state, &command).unwrap(), "cd");
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(state.current().path(), "/W.Workspace");
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap()["result"]["message"],
        "Switched to 'W.Workspace'"
    );
    assert_eq!(
        fs::read_to_string(state.context_file(&store)).unwrap(),
        "{\"path\": \"/W.Workspace\"}"
    );
    let mut reloaded = fixture.state(true);
    assert!(reloaded.restore(&store, &fixture.catalog).unwrap());
    assert_eq!(reloaded.current().path(), "/W.Workspace");
    fixture
        .run(&mut state, &parse(&["consumer", "cd", "Note.Notebook"]))
        .unwrap();
    assert_eq!(state.current().path(), "/W.Workspace/Note.Notebook");
    fixture
        .run(&mut state, &parse(&["consumer", "cd", "~"]))
        .unwrap();
    assert_eq!(state.current().path(), "/Home.Personal");
    let (stdout, _, _) = output(
        fixture
            .run(&mut state, &parse(&["consumer", "cd", "/"]))
            .unwrap(),
        "cd",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap()["result"]["message"],
        "Switched to root"
    );
}

#[test]
fn cd_default_and_interactive_modes_mutate_memory_without_persisting() {
    let mut fixture = Fixture::new();
    let store = fixture.store();
    let mut state = fixture.state(true);
    fixture
        .run(&mut state, &parse(&["consumer", "cd", "/W.Workspace"]))
        .unwrap();
    assert_eq!(state.current().path(), "/W.Workspace");
    assert!(!state.context_file(&store).exists());
    let original = Arc::clone(state.current());
    assert!(
        fixture
            .run(
                &mut state,
                &parse(&["consumer", "cd", "/Missing.Workspace"])
            )
            .is_err()
    );
    assert_eq!(*state.current(), original);
    execute(
        &LocalCommand::ConfigSet {
            key: "context_persistence_enabled".into(),
            value: "true".into(),
        },
        &store,
        &mut fixture,
    )
    .unwrap();
    let mut interactive = fixture.state(false);
    fixture
        .run(
            &mut interactive,
            &parse(&["consumer", "cd", "/W.Workspace"]),
        )
        .unwrap();
    assert!(!interactive.context_file(&store).exists());
}

#[test]
fn pwd_outputs_source_metadata_on_stderr_even_for_json_selection() {
    let mut fixture = Fixture::new();
    let mut state = fixture.state(true);
    fixture
        .run(
            &mut state,
            &parse(&["consumer", "cd", "/W.Workspace/Note.Notebook"]),
        )
        .unwrap();
    let (stdout, stderr, code) = output(
        fixture
            .run(&mut state, &parse(&["consumer", "pwd"]))
            .unwrap(),
        "pwd",
    );
    assert!(stdout.is_empty());
    assert_eq!(code, 0);
    let data: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(
        data,
        json!({"name":"Note", "type":".Notebook", "id":"note-id", "local_path": fixture.root.to_str().unwrap()})
    );
    assert!(stderr.starts_with(b"{\n    \"name\": \"Note\","));
}

#[test]
fn open_uses_source_routes_experience_claim_order_and_stub_launcher() {
    let mut fixture = Fixture::new();
    let mut state = fixture.state(true);
    let command = parse(&["consumer", "open", "/W.Workspace/Note.Notebook"]);
    let (stdout, stderr, code) = output(fixture.run(&mut state, &command).unwrap(), "open");
    assert_eq!(code, 0);
    let expected = "https://app.fabric.microsoft.com/groups/workspace-id/synapsenotebooks/note-id/?experience=fabric-developer";
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap()["result"]["message"],
        expected
    );
    assert_eq!(
        fixture.browser.0,
        [format!(
            "{expected}&upn=synthetic@example.invalid&ctid=tenant-id"
        )]
    );
    assert_eq!(stderr, b"Opening 'Note.Notebook' in the web browser...\n");
    let store = fixture.store();
    execute(
        &LocalCommand::ConfigSet {
            key: "default_open_experience".into(),
            value: "powerbi".into(),
        },
        &store,
        &mut fixture,
    )
    .unwrap();
    fixture
        .run(&mut state, &parse(&["consumer", "open", "/W.Workspace"]))
        .unwrap();
    assert_eq!(
        fixture.browser.0[1],
        "https://app.fabric.microsoft.com/groups/workspace-id/list?experience=power-bi&upn=synthetic@example.invalid&ctid=tenant-id"
    );
    fixture.user = false;
    assert!(fixture.run(&mut state, &command).is_err());
    assert_eq!(fixture.browser.0.len(), 2);
    fixture.user = true;
    fixture.browser_failure = true;
    assert!(fixture.run(&mut state, &command).is_err());
}

#[test]
fn clear_alias_calls_real_terminal_writer_and_writer_errors_propagate() {
    let mut fixture = Fixture::new();
    let mut state = fixture.state(true);
    assert_eq!(parse(&["consumer", "cls"]), LocalCommand::Clear);
    assert_eq!(
        output(
            fixture
                .run(&mut state, &parse(&["consumer", "clear"]))
                .unwrap(),
            "clear"
        ),
        (Vec::new(), Vec::new(), 0)
    );
    let terminal = std::mem::replace(&mut fixture.terminal, AnsiTerminal::new(Vec::new()));
    assert_eq!(terminal.into_inner(), b"\x1b[H\x1b[2J\x1b[3J");
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("terminal failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(AnsiTerminal::new(Broken).clear().is_err());
}

#[test]
fn extension_is_a_successful_unsupported_warning_not_a_plugin_stub() {
    let mut fixture = Fixture::new();
    let mut state = fixture.state(true);
    assert_eq!(
        output(
            fixture
                .run(&mut state, &parse(&["consumer", "extension"]))
                .unwrap(),
            "extension"
        ),
        (
            Vec::new(),
            b"! extension: Feature is not supported\n".to_vec(),
            0
        )
    );
}

#[test]
fn desc_uses_full_pinned_support_matrix_for_suffix_and_real_path() {
    let mut fixture = Fixture::new();
    let mut state = fixture.state(true);
    let command = parse(&["consumer", "desc", ".notebook"]);
    let (stdout, stderr, code) = output(fixture.run(&mut state, &command).unwrap(), "desc");
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let expected: String =
        serde_json::from_str(include_str!("fixtures/notebook-desc.json")).unwrap();
    assert_eq!(String::from_utf8(stdout).unwrap(), expected);
    let matrix = SupportMatrix::source().unwrap();
    assert_eq!(
        matrix.source_commit,
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    for suffix in &matrix.elements {
        assert!(matrix.model(suffix).is_ok(), "{suffix}");
    }
    let (stdout, _, _) = output(
        fixture
            .run(
                &mut state,
                &parse(&["consumer", "desc", "/W.Workspace/Note.Notebook"]),
            )
            .unwrap(),
        "desc",
    );
    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .starts_with("Commands for 'Note.Notebook'.")
    );
    let (stdout, stderr, code) = output(
        fixture
            .run(&mut state, &parse(&["consumer", "desc", "all"]))
            .unwrap(),
        "desc",
    );
    assert_eq!(code, 2);
    assert!(
        String::from_utf8(stdout)
            .unwrap()
            .contains("Available elements:\n")
    );
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("unknown Fabric element")
    );
}

#[test]
fn context_io_failures_leave_previous_state_and_corrupt_file_untouched() {
    let mut fixture = Fixture::new();
    let store = fixture.store();
    let mut state = fixture.state(true);
    execute(
        &LocalCommand::ConfigSet {
            key: "context_persistence_enabled".into(),
            value: "true".into(),
        },
        &store,
        &mut fixture,
    )
    .unwrap();
    let file = state.context_file(&store);
    fs::create_dir(&file).unwrap();
    let original = Arc::clone(state.current());
    assert!(
        fixture
            .run(&mut state, &parse(&["consumer", "cd", "/W.Workspace"]))
            .is_err()
    );
    assert_eq!(*state.current(), original);
    fs::remove_dir(&file).unwrap();
    fs::write(&file, b"{broken").unwrap();
    assert!(state.restore(&store, &fixture.catalog).is_err());
    assert_eq!(fs::read(&file).unwrap(), b"{broken");
}
