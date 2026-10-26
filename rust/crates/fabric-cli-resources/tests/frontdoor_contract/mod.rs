// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_output::text::{OutputFormat, ascii_width};
use fabric_cli_resources::{
    CommandError, FamilyPresentation, FamilySurface,
    frontdoor::{
        AccessCommand, AccessHost, AccessKind, AccessTargets, ApiClients, register, render,
    },
};
use fabric_sdk_api::RequestBounds;
use std::{future::Future, io, path::PathBuf, pin::Pin};

struct Targets {
    element: Arc<Element>,
    calls: AtomicUsize,
}

impl AccessTargets for Targets {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Arc<Element>, CommandError>> + Send + 'a>> {
        Box::pin(async move {
            assert!(!path.is_empty());
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.element.clone())
        })
    }
}

fn target(item: Option<&str>) -> Targets {
    let workspace = Element::child(
        "Workspace",
        Some(
            if item.is_some() {
                "workspace"
            } else {
                "resource"
            }
            .into(),
        ),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("offline", "tenant"),
    )
    .unwrap();
    Targets {
        element: match item {
            None => workspace,
            Some(kind) => Element::child(
                "Item",
                Some("resource".into()),
                NodeKind::Item(ItemType::parse(kind).unwrap()),
                workspace,
            )
            .unwrap(),
        },
        calls: AtomicUsize::new(0),
    }
}

fn parsed(args: &[String]) -> AccessCommand {
    let matches = register(clap::Command::new("frontdoor-consumer"))
        .try_get_matches_from(
            std::iter::once("frontdoor-consumer".to_owned()).chain(args.iter().cloned()),
        )
        .unwrap();
    AccessCommand::from_matches(&matches).unwrap().unwrap()
}

fn parse(args: &[&str]) -> AccessCommand {
    parsed(&args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>())
}

fn host<'a>(api: &'a ApiClient, targets: &'a Targets) -> AccessHost<'a> {
    AccessHost {
        clients: ApiClients {
            fabric: api,
            onelake: None,
            arm: None,
            powerbi: None,
        },
        targets,
        label_definitions: None,
        multipart_boundary: "offline-boundary",
        limits: Limits::default(),
        request_bounds: RequestBounds::default(),
    }
}

struct Definitions(PathBuf);
impl Definitions {
    fn new(labels: &Value) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "fabric-access-frontdoor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("labels.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"labels":labels})).unwrap(),
        )
        .unwrap();
        Self(path)
    }
}
impl Drop for Definitions {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).unwrap();
        std::fs::remove_dir(self.0.parent().unwrap()).unwrap();
    }
}

#[test]
fn composed_schema_preserves_aliases_cardinality_quotes_and_command_value_collisions() {
    let command = parse(&["acl", "dir", "A", "B.Workspace", "-q", "'name'"]);
    assert!(
        matches!(command.kind, AccessKind::AclList { path, query: Some(query), .. }
        if path == ["A", "B.Workspace"] && query == "'name'")
    );
    let command = parse(&["acl", "get", "A.Workspace", "-q", "'name", "|", "keys(@)'"]);
    assert!(
        matches!(command.kind, AccessKind::AclGet { query: Some(query), .. }
        if query == "name | keys(@)")
    );
    let command = parse(&["find", "api", "-P", "type=Notebook", "-q", "[]"]);
    assert!(matches!(command.kind, AccessKind::Find(options)
        if options.search == "api" && options.params.as_deref() == Some("type=Notebook")));
    let command = parse(&[
        "api",
        "acl",
        "-i",
        "{\"name\":",
        "\"find\"}",
        "-A",
        "powerbi",
    ]);
    assert!(matches!(command.kind, AccessKind::Api(options)
        if options.origin == fabric_sdk_api::Origin::PowerBi && options.endpoint == "acl"
        && options.input == ["{\"name\":", "\"find\"}"]));
    for args in [
        vec!["acl", "ls"],
        vec!["label", "list-local", "--name", "unexpected"],
        vec!["find", "data", "-q"],
        vec!["api", "items", "-X", "GET"],
        vec!["api", "items", "-A", "inferred"],
    ] {
        assert!(
            register(clap::Command::new("test"))
                .try_get_matches_from(std::iter::once("test").chain(args),)
                .is_err()
        );
    }
    let root = register(clap::Command::new("test")).subcommand(clap::Command::new("other"));
    let matches = root.try_get_matches_from(["test", "other"]).unwrap();
    assert!(AccessCommand::from_matches(&matches).unwrap().is_none());
}

#[tokio::test]
async fn five_read_leaves_parse_execute_and_render_unchanged_python_stream_fixtures() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/access/render_cases.json"
    ))
    .unwrap();
    let mut leaves = std::collections::BTreeSet::new();
    for case in fixture["cases"].as_array().unwrap() {
        let kind = case["kind"].as_str().unwrap();
        if kind == "error"
            || kind == "label-remove"
            || !case["expected"]["error"].is_null()
            || (kind == "acl" && !matches!(case["resource"].as_str(), Some("workspace" | "item")))
        {
            continue;
        }
        let mut args: Vec<String> = match kind {
            "acl" => vec![
                "acl".into(),
                case["command"].as_str().unwrap().into(),
                "Workspace.Workspace".into(),
            ],
            "label-local" => vec!["label".into(), "list-local".into()],
            "find" => vec!["find".into(), "report".into()],
            "api" => vec!["api".into(), "synthetic".into()],
            _ => panic!("unexpected source fixture"),
        };
        leaves.insert(args[..args.len().min(2)].join(" "));
        if case["long"] == true {
            args.push("-l".into());
        }
        if let Some(query) = case["query"].as_str() {
            args.extend(["-q".into(), query.into()]);
        }
        args.extend([
            "--output_format".into(),
            case["format"].as_str().unwrap().into(),
        ]);
        let command = parsed(&args);
        let (api, wire, credential) = setup(vec![reply(
            200,
            &case.get("body").cloned().unwrap_or(json!({})).to_string(),
            &[],
        )]);
        let targets = target((case["resource"] == "item").then_some("Notebook"));
        let definitions = (kind == "label-local").then(|| Definitions::new(&case["labels"]));
        let mut host = host(&api, &targets);
        host.label_definitions = definitions.as_ref().map(|d| d.0.as_path());
        let result = host.execute(&command, pending()).await;
        let mut presentation = FamilyPresentation::new(OutputFormat::Json, FamilySurface::Api);
        presentation.terminal_width = Some(48);
        let output = render(
            &command,
            result,
            Timestamp::parse("2026-01-02T03:04:05.000006Z").unwrap(),
            &presentation,
            &|c| {
                if c == '\u{2026}' {
                    Some(1)
                } else {
                    ascii_width(c)
                }
            },
        )
        .unwrap_or_else(|e| panic!("{}: {e}", case["name"]));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            output.write_to(&mut stdout, &mut stderr).unwrap(),
            case["expected"]["exit"].as_u64().unwrap() as u8
        );
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            case["expected"]["stdout"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            case["expected"]["stderr"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        let requests: Vec<_> = wire
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| json!({"method":r.method().to_string(),"url":r.url().as_str()}))
            .collect();
        assert_eq!(
            json!(requests),
            case["expected"]["requests"],
            "{}",
            case["name"]
        );
        assert_eq!(
            credential.calls.load(Ordering::SeqCst),
            usize::from(kind != "label-local")
        );
        assert_eq!(
            targets.calls.load(Ordering::SeqCst),
            usize::from(kind == "acl")
        );
    }
    assert_eq!(leaves.len(), 5);
}

#[tokio::test]
async fn find_uses_source_filters_query_and_exact_paged_request_bodies() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/catalog/cases.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let mut args = vec!["find".into(), "needle".into()];
        if let Some(params) = case["params"].as_str() {
            args.extend(["-P".into(), params.into()]);
        }
        if case["long"] == true {
            args.push("-l".into());
        }
        if let Some(query) = case["query"].as_str() {
            args.extend(["-q".into(), query.into()]);
        }
        let command = parsed(&args);
        let replies = case["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                reply(
                    r["status"].as_u64().unwrap() as u16,
                    &r["body"].to_string(),
                    &[],
                )
            })
            .collect();
        let (api, wire, _) = setup(replies);
        let targets = target(None);
        let result = host(&api, &targets).execute(&command, pending()).await;
        if case["expected"]["error"].is_null() {
            let value = result.unwrap();
            assert_eq!(
                value.data.unwrap_or(Value::Null),
                case["expected"]["data"],
                "{}",
                case["name"]
            );
            assert_eq!(json!(value.diagnostics), case["expected"]["diagnostics"]);
        } else {
            assert_eq!(
                result.unwrap_err().code,
                case["expected"]["error"]["code"].as_str().unwrap()
            );
        }
        let requests: Vec<_> = wire
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                json!({"method":r.method().to_string(),"url":r.url().as_str(),
                "body":serde_json::from_slice::<Value>(&azure_core::Bytes::from(r.body())).unwrap()})
            })
            .collect();
        assert_eq!(
            json!(requests),
            case["expected"]["requests"],
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn raw_api_selects_explicit_public_origin_scope_and_rejects_unsafe_headers_before_credentials()
 {
    for (audience, scope, url) in [
        (
            "fabric",
            Audience::Fabric,
            "https://api.fabric.microsoft.com/v1/resources",
        ),
        (
            "storage",
            Audience::OneLake,
            "https://onelake.dfs.fabric.microsoft.com/resources",
        ),
        (
            "azure",
            Audience::Arm,
            "https://management.azure.com/resources",
        ),
        (
            "powerbi",
            Audience::Fabric,
            "https://api.powerbi.com/v1.0/myorg/resources",
        ),
    ] {
        let (api, wire, credential) =
            setup_audience(vec![reply(403, "{\"error\":\"denied\"}", &[])], scope);
        let targets = target(None);
        let mut host = host(&api, &targets);
        host.clients.onelake = Some(&api);
        host.clients.arm = Some(&api);
        host.clients.powerbi = Some(&api);
        let command = parse(&["api", "resources", "-A", audience, "-q", "text.error"]);
        let value = host.execute(&command, pending()).await.unwrap();
        assert_eq!(value.data, Some(json!("denied")));
        assert_eq!(wire.requests.lock().unwrap()[0].url().as_str(), url);
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        let command = parse(&[
            "api",
            "resources",
            "-A",
            audience,
            "-H",
            "Authorization=forbidden",
        ]);
        assert_eq!(
            host.execute(&command, pending()).await.unwrap_err().code,
            "InvalidInput"
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn unsupported_capabilities_cancellation_and_malformed_data_never_report_success() {
    let (api, wire, credential) = setup(vec![reply(200, "{\"wrong\":[]}", &[])]);
    let targets = target(None);
    let mut host = host(&api, &targets);
    for args in [
        vec!["acl", "get", "Workspace.Workspace", "-o", "result.json"],
        vec!["api", "resources", "--skill", "unbound"],
        vec!["api", "resources", "-A", "storage"],
    ] {
        assert!(host.execute(&parse(&args), pending()).await.is_err());
    }
    assert!(
        host.execute(&parse(&["acl", "ls", "Workspace.Workspace"]), ready(()))
            .await
            .is_err()
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
    let command = parse(&["find", "needle", "--output_format", "json"]);
    let error = host.execute(&command, pending()).await.unwrap_err();
    let presentation = FamilyPresentation::new(OutputFormat::Json, FamilySurface::Api);
    let output = render(
        &command,
        Err(error),
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &presentation,
        &ascii_width,
    )
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 1);
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
}

struct BrokenWriter;
impl io::Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected writer failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn parsed_result_propagates_writer_failure_instead_of_returning_exit_zero() {
    let command = parse(&["api", "resources", "--output_format", "json"]);
    let (api, _, _) = setup(vec![reply(200, "{}", &[])]);
    let targets = target(None);
    let result = host(&api, &targets).execute(&command, pending()).await;
    let output = render(
        &command,
        result,
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &FamilyPresentation::new(OutputFormat::Text, FamilySurface::Get),
        &ascii_width,
    )
    .unwrap();
    assert!(output.write_to(&mut BrokenWriter, &mut Vec::new()).is_err());
}
