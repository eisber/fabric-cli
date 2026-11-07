// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ID-addressed viewer routing with no metadata, configuration, or identity lookup.
//!
//! Route bindings are the explicit pinned table from
//! `fabric-cli-local/src/support.json` at integration checkpoint
//! `2970ce80e77dcd7900c62c50e8a3c0fc8690187e`. They are routing support,
//! not proof of remote membership, authorization, browser identity or display.

use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::{CliError, Control, EffectKind, EffectState, OperationResult, Status, check_control};

const PREFIX: &str = "https://app.fabric.microsoft.com/groups/";
const LAUNCH_LIMIT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Explicit item type-to-viewer routes from the pinned source, without fallback.
pub const SUPPORTED_ITEM_ROUTES: &[(&str, &str)] = &[
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

/// Caller-owned viewer capability; success acknowledges launch, never display.
pub trait ViewerLauncher {
    /// Ask the viewer to open one validated URI using the caller's control policy.
    ///
    /// An error before launch should carry `ViewerLaunch/NotStarted`. If the
    /// request may have escaped, retain `Unknown` or stronger observed evidence.
    /// The caller conservatively treats an error without launch evidence as
    /// unknown. Implementations must not retry the launch or claim display.
    fn launch(&mut self, uri: &str, control: &impl Control) -> Result<(), CliError>;
}

/// Native system-browser launcher with separated arguments and no shell string.
///
/// The child is observed for at most 30 seconds in addition to the caller's
/// cancellation/deadline. On interruption only this owned launcher child is
/// terminated; browser processes, descendants, or process names are never killed.
/// A request may already have reached the browser and remains an unknown effect.
pub struct SystemViewer;

impl ViewerLauncher for SystemViewer {
    fn launch(&mut self, uri: &str, control: &impl Control) -> Result<(), CliError> {
        if !supported_uri(uri) {
            return Err(not_started(
                "InvalidInput",
                "The native viewer only accepts a supported Fabric resource URI.",
            ));
        }
        check_control(control).map_err(with_not_started)?;
        let mut command = native_command(uri)?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| {
            not_started(
                "ViewerUnavailable",
                "The native viewer launcher could not be started.",
            )
        })?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => {
                    return Err(unknown(
                        "ViewerLaunchFailed",
                        "The launcher failed after starting; browser delivery is unconfirmed.",
                    ));
                }
                Ok(None) => {}
                Err(_) => {
                    stop_launcher(child);
                    return Err(unknown(
                        "OutcomeUnknown",
                        "The launcher could not be observed; browser delivery is unconfirmed.",
                    ));
                }
            }
            if let Err(error) = check_control(control) {
                stop_launcher(child);
                return Err(error.with_effect(EffectKind::ViewerLaunch, EffectState::Unknown));
            }
            if started.elapsed() >= LAUNCH_LIMIT {
                stop_launcher(child);
                return Err(unknown(
                    "Timeout",
                    "The launcher exceeded its 30-second limit; browser delivery is unconfirmed.",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Route an explicit GUID-addressed workspace/item and hand off or launch once.
///
/// Accepted paths are `/<guid>.Workspace` and
/// `/<guid>.Workspace/<guid>.<SupportedType>` only. Names, representations,
/// subresources and local paths are not guessed or resolved through live APIs.
/// `experience` must be exactly `fabric` or `powerbi`. URI-only mode returns a
/// manual handoff and never calls the launcher. No identity hints are emitted.
///
/// # Errors
/// Rejects invalid/unsupported targets and experiences, cancellation/deadlines,
/// or launcher failures. Any possible launch remains attached to the error.
pub fn run(
    path: &str,
    experience: &str,
    uri_only: bool,
    launcher: &mut impl ViewerLauncher,
    control: &impl Control,
) -> Result<OperationResult, CliError> {
    let experience_route = match experience {
        "fabric" => "fabric-developer",
        "powerbi" => "power-bi",
        _ => {
            return Err(CliError::new(
                "InvalidInput",
                "Experience must be fabric or powerbi.",
            ));
        }
    };
    let target = Target::parse(path)?;
    let uri = target.uri(experience_route);
    check_control(control)?;
    let data = json!({
        "uri": uri,
        "workspace_id": target.workspace_id,
        "item_id": target.item_id,
        "item_type": target.item_type,
        "experience": experience,
        "metadata_verified": false,
        "display_verified": false,
    });
    if uri_only {
        return Ok(OperationResult::new(Status::Handoff, data));
    }
    launcher.launch(&uri, control).map_err(|mut error| {
        if !error
            .effects
            .iter()
            .any(|effect| effect.kind == EffectKind::ViewerLaunch)
        {
            error = error.with_effect(EffectKind::ViewerLaunch, EffectState::Unknown);
        }
        error
    })?;
    check_control(control)
        .map_err(|error| error.with_effect(EffectKind::ViewerLaunch, EffectState::Acknowledged))?;
    Ok(OperationResult::new(Status::Accepted, data)
        .with_effect(EffectKind::ViewerLaunch, EffectState::Acknowledged))
}

struct Target<'a> {
    workspace_id: String,
    item_id: Option<String>,
    item_type: Option<&'a str>,
    route: Option<&'static str>,
}

impl<'a> Target<'a> {
    fn parse(path: &'a str) -> Result<Self, CliError> {
        let invalid = || {
            CliError::new(
                "InvalidInput",
                "Use an absolute GUID-addressed workspace or supported item path.",
            )
        };
        let relative = path.strip_prefix('/').ok_or_else(invalid)?;
        let mut segments = relative.split('/');
        let workspace = segments.next().ok_or_else(invalid)?;
        let workspace_id = workspace
            .strip_suffix(".Workspace")
            .filter(|id| guid(id))
            .ok_or_else(invalid)?;
        let mut target = Self {
            workspace_id: workspace_id.to_ascii_lowercase(),
            item_id: None,
            item_type: None,
            route: None,
        };
        if let Some(item) = segments.next() {
            let (id, kind) = item.rsplit_once('.').ok_or_else(invalid)?;
            if !guid(id) || segments.next().is_some() {
                return Err(invalid());
            }
            let route = SUPPORTED_ITEM_ROUTES
                .iter()
                .find_map(|&(supported, route)| (supported == kind).then_some(route))
                .ok_or_else(|| {
                    CliError::new(
                        "UnsupportedTarget",
                        "This item has no admitted viewer route.",
                    )
                })?;
            target.item_id = Some(id.to_ascii_lowercase());
            target.item_type = Some(kind);
            target.route = Some(route);
        }
        Ok(target)
    }

    fn uri(&self, experience: &str) -> String {
        match (&self.item_id, self.route) {
            (Some(id), Some(route)) => format!(
                "{PREFIX}{}/{route}/{id}/?experience={experience}",
                self.workspace_id,
            ),
            _ => format!("{PREFIX}{}/list?experience={experience}", self.workspace_id),
        }
    }
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
}

fn supported_uri(uri: &str) -> bool {
    let Some(relative) = uri.strip_prefix(PREFIX) else {
        return false;
    };
    let Some((path, experience)) = relative.split_once("?experience=") else {
        return false;
    };
    if !matches!(experience, "fabric-developer" | "power-bi") {
        return false;
    }
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        [workspace, "list"] => guid(workspace),
        [workspace, route, item, ""] => {
            guid(workspace)
                && guid(item)
                && SUPPORTED_ITEM_ROUTES
                    .iter()
                    .any(|&(_, supported)| supported == *route)
        }
        _ => false,
    }
}

fn with_not_started(error: CliError) -> CliError {
    error.with_effect(EffectKind::ViewerLaunch, EffectState::NotStarted)
}

fn not_started(code: &'static str, message: &'static str) -> CliError {
    with_not_started(CliError::new(code, message))
}

fn unknown(code: &'static str, message: &'static str) -> CliError {
    CliError::new(code, message).with_effect(EffectKind::ViewerLaunch, EffectState::Unknown)
}

fn native_command(uri: &str) -> Result<Command, CliError> {
    #[cfg(windows)]
    let mut command = {
        let root = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .filter(|root| root.is_absolute())
            .ok_or_else(|| {
                not_started(
                    "ViewerUnavailable",
                    "An absolute Windows system root is required.",
                )
            })?;
        let mut command = Command::new(root.join("System32").join("rundll32.exe"));
        command.arg(
            root.join("System32")
                .join("url.dll")
                .to_string_lossy()
                .to_string()
                + ",FileProtocolHandler",
        );
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    return Err(not_started(
        "UnsupportedCapability",
        "No system viewer adapter exists for this platform.",
    ));
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    {
        command.arg(uri);
        Ok(command)
    }
}

fn stop_launcher(mut child: Child) {
    // Only the directly owned launcher is touched. Its downstream request cannot
    // be recalled, so callers retain Unknown even when termination succeeds.
    let _ = child.kill();
    if !matches!(child.try_wait(), Ok(Some(_))) {
        // Reaping must not extend the caller's cancellation/deadline. A failed
        // reaper allocation does not turn the already-unknown effect into success.
        let _ = thread::Builder::new()
            .name("fabric-viewer-reap".to_owned())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}

#[cfg(test)]
#[deny(unnameable_test_items)]
mod tests {
    use super::*;

    #[test]
    fn native_uri_guard_rejects_noncanonical_hosts_routes_and_query_injection() {
        for uri in [
            "file:///etc/passwd",
            "https://app.fabric.microsoft.com.evil.invalid/groups/x",
            "https://app.fabric.microsoft.com/groups/11111111-2222-3333-4444-555555555555/list?experience=fabric-developer&upn=user",
            "https://app.fabric.microsoft.com/groups/11111111-2222-3333-4444-555555555555/unknown/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/?experience=power-bi",
        ] {
            assert!(!supported_uri(uri));
        }
        assert!(supported_uri(
            "https://app.fabric.microsoft.com/groups/11111111-2222-3333-4444-555555555555/list?experience=fabric-developer"
        ));
    }

    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    #[test]
    fn native_arguments_are_separate_and_never_a_shell_script() {
        let uri = "https://app.fabric.microsoft.com/groups/11111111-2222-3333-4444-555555555555/list?experience=power-bi";
        let command = native_command(uri).unwrap();
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments.last().unwrap(), &std::ffi::OsStr::new(uri));
        #[cfg(windows)]
        {
            assert!(std::path::Path::new(command.get_program()).is_absolute());
            assert_eq!(arguments.len(), 2);
            assert!(
                arguments[0]
                    .to_string_lossy()
                    .ends_with("url.dll,FileProtocolHandler")
            );
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(command.get_program(), "/usr/bin/open");
            assert_eq!(arguments.len(), 1);
        }
        #[cfg(target_os = "linux")]
        {
            assert_eq!(command.get_program(), "xdg-open");
            assert_eq!(arguments.len(), 1);
        }
    }
}
