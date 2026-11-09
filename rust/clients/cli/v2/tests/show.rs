// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline URI and injected-launch contracts; no native browser is invoked.

#![deny(unnameable_test_items)]

use std::cell::Cell;

use fabric_cli_v2::{
    CliError, Control, EffectKind, EffectState, Status,
    show::{SUPPORTED_ITEM_ROUTES, SystemViewer, ViewerLauncher, run},
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

struct State {
    cancelled: Cell<bool>,
    expired: bool,
}

impl State {
    fn running() -> Self {
        Self {
            cancelled: Cell::new(false),
            expired: false,
        }
    }
}

impl Control for State {
    fn is_cancelled(&self) -> bool {
        self.cancelled.get()
    }
    fn deadline_elapsed(&self) -> bool {
        self.expired
    }
}

#[derive(Default)]
struct Capture {
    urls: Vec<String>,
    failure: Option<CliError>,
}

impl ViewerLauncher for Capture {
    fn launch(&mut self, uri: &str, _: &impl Control) -> Result<(), CliError> {
        self.urls.push(uri.to_owned());
        self.failure.take().map_or(Ok(()), Err)
    }
}

fn workspace() -> String {
    format!("/{WORKSPACE}.Workspace")
}
fn item(kind: &str) -> String {
    format!("/{WORKSPACE}.Workspace/{ITEM}.{kind}")
}

#[test]
fn workspace_routes_and_handoffs_have_no_launch_identity_or_display_claim() {
    let mut launcher = Capture::default();
    for (experience, route) in [("fabric", "fabric-developer"), ("powerbi", "power-bi")] {
        let result = run(
            &workspace(),
            experience,
            true,
            &mut launcher,
            &State::running(),
        )
        .unwrap();
        assert_eq!(result.status, Status::Handoff);
        assert!(result.effects.is_empty());
        assert_eq!(
            result.data["uri"],
            format!("https://app.fabric.microsoft.com/groups/{WORKSPACE}/list?experience={route}")
        );
        assert_eq!(result.data["workspace_id"], WORKSPACE);
        assert!(result.data["item_id"].is_null());
        assert_eq!(result.data["metadata_verified"], false);
        assert_eq!(result.data["display_verified"], false);
        let uri = result.data["uri"].as_str().unwrap();
        assert!(!uri.contains("upn="));
        assert!(!uri.contains("ctid="));
    }
    assert!(launcher.urls.is_empty());
}

#[test]
fn all_pinned_item_routes_are_exact_and_both_experiences_are_supported() {
    let expected = [
        ("AISkill", "aiskills"),
        ("ApacheAirflowJob", "apacheairflowprojects"),
        ("Exploration", "explorations"),
        ("RetailDataManager", "retail-data-manager"),
        ("Healthcaredatasolution", "health-data-manager"),
        ("SustainabilityDataSolution", "sustainability-data-manager"),
        ("MetricSet", "metricsets"),
        ("OrgApp", "orgapps"),
        ("CosmosDBDatabase", "cosmosdbdatabases"),
        ("Dashboard", "dashboards"),
        ("Datamart", "datamarts"),
        ("DataPipeline", "pipelines"),
        ("DigitalTwinBuilder", "digital-twin-builder"),
        ("DigitalTwinBuilderFlow", "digital-twin-builder-flow"),
        ("Environment", "sparkenvironments"),
        ("Eventhouse", "eventhouses"),
        ("Eventstream", "eventstreams"),
        ("GraphModel", "graph"),
        ("KQLDashboard", "kustodashboards"),
        ("KQLDatabase", "databases"),
        ("KQLQueryset", "queryworkbenches"),
        ("Lakehouse", "lakehouses"),
        ("Map", "maps"),
        ("MirroredDatabase", "mirroreddatabases"),
        ("MLExperiment", "mlexperiments"),
        ("MLModel", "mlmodels"),
        ("Notebook", "synapsenotebooks"),
        ("Ontology", "ontologies"),
        ("PaginatedReport", "rdlreports"),
        ("Reflex", "reflexes"),
        ("Report", "reports"),
        ("SemanticModel", "datasets"),
        ("SparkJobDefinition", "sparkjobdefinitions"),
        ("SQLDatabase", "sqldatabases"),
        ("SQLEndpoint", "lakewarehouses"),
        ("Warehouse", "datawarehouses"),
        ("CopyJob", "copyjobs"),
        ("GraphQuerySet", "graph-queryset"),
        ("UserDataFunction", "userdatafunctions"),
        ("GraphQLApi", "graphql"),
        ("MountedDataFactory", "mounteddatafactories"),
        ("Dataflow", "dataflows-gen2"),
        ("VariableLibrary", "variable-libraries"),
    ];
    assert_eq!(SUPPORTED_ITEM_ROUTES, expected.as_slice());
    let mut launcher = Capture::default();
    for (kind, route) in expected {
        for (experience, query) in [("fabric", "fabric-developer"), ("powerbi", "power-bi")] {
            let result = run(
                &item(kind),
                experience,
                true,
                &mut launcher,
                &State::running(),
            )
            .unwrap();
            assert_eq!(
                result.data["uri"],
                format!(
                    "https://app.fabric.microsoft.com/groups/{WORKSPACE}/{route}/{ITEM}/?experience={query}"
                )
            );
            assert_eq!(result.data["workspace_id"], WORKSPACE);
            assert_eq!(result.data["item_id"], ITEM);
            assert_eq!(result.data["item_type"], kind);
        }
    }
    assert!(launcher.urls.is_empty());
}

#[test]
fn launch_returns_acknowledgement_once_and_result_serialization_never_relaunches() {
    let mut launcher = Capture::default();
    let result = run(
        &item("Notebook"),
        "fabric",
        false,
        &mut launcher,
        &State::running(),
    )
    .unwrap();
    assert_eq!(result.status, Status::Accepted);
    assert_eq!(result.effects.len(), 1);
    assert_eq!(result.effects[0].kind, EffectKind::ViewerLaunch);
    assert_eq!(result.effects[0].state, EffectState::Acknowledged);
    assert_eq!(
        launcher.urls,
        [format!(
            "https://app.fabric.microsoft.com/groups/{WORKSPACE}/synapsenotebooks/{ITEM}/?experience=fabric-developer"
        )]
    );
    let encoded = serde_json::to_value(&result).unwrap();
    assert_eq!(encoded["effects"][0]["state"], "acknowledged");
    assert_eq!(encoded["data"]["display_verified"], false);
    assert_eq!(launcher.urls.len(), 1);
    struct BrokenWriter;
    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected output failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(serde_json::to_writer(BrokenWriter, &result).is_err());
    assert_eq!(result.effects[0].state, EffectState::Acknowledged);
    assert_eq!(launcher.urls.len(), 1);
}

#[test]
fn uuid_case_is_canonicalized_but_type_spelling_is_not_guessed() {
    let mut launcher = Capture::default();
    let path = item("Notebook")
        .to_ascii_uppercase()
        .replace(".WORKSPACE", ".Workspace")
        .replace(".NOTEBOOK", ".Notebook");
    let result = run(&path, "fabric", true, &mut launcher, &State::running()).unwrap();
    assert_eq!(result.data["item_id"], ITEM);
    assert!(
        run(
            &item("notebook"),
            "fabric",
            false,
            &mut launcher,
            &State::running()
        )
        .is_err()
    );
    assert!(launcher.urls.is_empty());
}

#[test]
fn names_invalid_guids_subresources_local_paths_and_unknown_types_never_launch() {
    let mut launcher = Capture::default();
    let paths = [
        String::new(),
        "/".into(),
        "C:\\file.Notebook".into(),
        "file:///x".into(),
        "https://app.fabric.microsoft.com/groups/x".into(),
        "Demo.Workspace".into(),
        "/Demo.Workspace".into(),
        format!("/{WORKSPACE}.Personal"),
        format!("/{WORKSPACE}.workspace"),
        format!("//{WORKSPACE}.Workspace"),
        format!("{}/", workspace()),
        format!("/{WORKSPACE}.Workspace/{ITEM}.Unknown"),
        format!("/{WORKSPACE}.Workspace/{ITEM}.Workspace"),
        format!("{}/docs/overview.md", item("Notebook")),
        format!("{}/src/definition", item("Notebook")),
        format!("{}/Files/file", item("Lakehouse")),
        format!("{}/Tables/table", item("Lakehouse")),
        format!("{}/../{ITEM}.Notebook", workspace()),
        format!("{}/{ITEM}.Folder/{ITEM}.Notebook", workspace()),
        format!("{}/{ITEM}.Notebook?x=y", workspace()),
        format!("{}/{ITEM}.Notebook\n", workspace()),
        format!("{}/{ITEM}.Notebook#fragment", workspace()),
        format!("/00000000-0000-0000-0000-000000000000.Workspace/{ITEM}.Notebook"),
        format!("/{WORKSPACE}.Workspace/00000000-0000-0000-0000-000000000000.Notebook"),
        format!("/{WORKSPACE}.Workspace/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeg.Notebook"),
        format!("/{WORKSPACE}.Workspace/{{{ITEM}}}.Notebook"),
        format!("/{WORKSPACE}.Workspace/aaaaaaaa%2fbbbb-cccc-dddd-eeeeeeeeeeee.Notebook"),
    ];
    for path in paths {
        assert!(
            run(&path, "fabric", false, &mut launcher, &State::running()).is_err(),
            "{path:?}"
        );
    }
    for experience in [
        "",
        "Fabric",
        "power-bi",
        "fabric&upn=user",
        "https://evil.invalid",
    ] {
        assert!(
            run(
                &workspace(),
                experience,
                false,
                &mut launcher,
                &State::running()
            )
            .is_err()
        );
    }
    assert!(launcher.urls.is_empty());
}

#[test]
fn cancellation_and_deadline_before_launch_or_handoff_prevent_effects() {
    let mut launcher = Capture::default();
    for uri_only in [true, false] {
        let control = State {
            cancelled: Cell::new(true),
            expired: false,
        };
        assert_eq!(
            run(&workspace(), "fabric", uri_only, &mut launcher, &control)
                .unwrap_err()
                .code,
            "Cancelled"
        );
        let control = State {
            cancelled: Cell::new(false),
            expired: true,
        };
        assert_eq!(
            run(&workspace(), "fabric", uri_only, &mut launcher, &control)
                .unwrap_err()
                .code,
            "Timeout"
        );
    }
    assert!(launcher.urls.is_empty());
}

#[test]
fn launcher_failures_preserve_known_evidence_or_conservatively_report_unknown() {
    for known in [
        None,
        Some(EffectState::NotStarted),
        Some(EffectState::Acknowledged),
    ] {
        let mut failure = CliError::new("InjectedFailure", "The injected launcher failed.");
        if let Some(state) = known {
            failure = failure.with_effect(EffectKind::ViewerLaunch, state);
        }
        let mut launcher = Capture {
            urls: Vec::new(),
            failure: Some(failure),
        };
        let error = run(
            &workspace(),
            "fabric",
            false,
            &mut launcher,
            &State::running(),
        )
        .unwrap_err();
        assert_eq!(error.code, "InjectedFailure");
        assert_eq!(error.effects.len(), 1);
        assert_eq!(
            error.effects[0].state,
            known.unwrap_or(EffectState::Unknown)
        );
        assert_eq!(launcher.urls.len(), 1);
    }
}

struct CancelAfterLaunch<'a> {
    state: &'a State,
    calls: usize,
}

impl ViewerLauncher for CancelAfterLaunch<'_> {
    fn launch(&mut self, _: &str, _: &impl Control) -> Result<(), CliError> {
        self.calls += 1;
        self.state.cancelled.set(true);
        Ok(())
    }
}

#[test]
fn cancellation_after_acknowledgement_never_implies_rollback_or_retries() {
    let state = State::running();
    let mut launcher = CancelAfterLaunch {
        state: &state,
        calls: 0,
    };
    let error = run(&workspace(), "fabric", false, &mut launcher, &state).unwrap_err();
    assert_eq!(error.code, "Cancelled");
    assert_eq!(error.effects[0].state, EffectState::Acknowledged);
    assert_eq!(launcher.calls, 1);
}

#[test]
fn native_viewer_rejects_bad_uri_or_prelaunch_cancellation_without_spawning() {
    let mut viewer = SystemViewer;
    let error = viewer
        .launch("https://evil.invalid", &State::running())
        .unwrap_err();
    assert_eq!(error.code, "InvalidInput");
    assert_eq!(error.effects[0].state, EffectState::NotStarted);
    let state = State {
        cancelled: Cell::new(true),
        expired: false,
    };
    let error = viewer.launch(
        &format!("https://app.fabric.microsoft.com/groups/{WORKSPACE}/list?experience=fabric-developer"),
        &state,
    ).unwrap_err();
    assert_eq!(error.code, "Cancelled");
    assert_eq!(error.effects[0].state, EffectState::NotStarted);
}
