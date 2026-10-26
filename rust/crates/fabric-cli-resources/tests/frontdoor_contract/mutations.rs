// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_cli_resources::{AclChange, Confirmation, LabelChange};

struct Prompt {
    answers: VecDeque<bool>,
    questions: Vec<String>,
}

impl Confirmation for Prompt {
    fn confirm<'a>(
        &'a mut self,
        question: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, CommandError>> + Send + 'a>> {
        Box::pin(async move {
            self.questions.push(question.into());
            Ok(self
                .answers
                .pop_front()
                .expect("unscripted source confirmation"))
        })
    }
}

#[test]
fn mutation_aliases_and_name_cardinality_use_the_source_registration() {
    assert!(matches!(
        parse(&["acl", "del", "Workspace.Workspace", "-I", "principal"]).kind,
        AccessKind::AclChange { change: AclChange::Remove { identity }, force: false, .. }
            if identity == "principal"
    ));
    assert!(matches!(
        parse(&["label", "del", "Workspace.Workspace/Item.Notebook", "-f"]).kind,
        AccessKind::LabelChange {
            change: LabelChange::Remove,
            force: true,
            ..
        }
    ));
    assert!(matches!(
        parse(&["label", "set", "Workspace.Workspace/Item.Notebook", "-n", "'Highly", "Confidential'"]).kind,
        AccessKind::LabelChange { change: LabelChange::Set(name), .. }
            if name == "Highly Confidential"
    ));
    assert!(
        register(clap::Command::new("test"))
            .try_get_matches_from([
                "test",
                "acl",
                "set",
                "Workspace.Workspace",
                "-I",
                "principal"
            ])
            .is_err()
    );
}

#[tokio::test]
async fn four_mutation_leaves_match_original_python_requests_prompts_messages_and_streams() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/access/mutation_cases.json"
    ))
    .unwrap();
    let streams: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/access/render_cases.json"
    ))
    .unwrap();
    let mut leaves = std::collections::BTreeSet::new();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| matches!(case["resource"].as_str(), Some("workspace" | "label")))
    {
        let label = case["resource"] == "label";
        let family = if label { "label" } else { "acl" };
        let mut args = vec![
            family.into(),
            case["command"].as_str().unwrap().into(),
            "Workspace.Workspace".into(),
        ];
        leaves.insert(args[..2].join(" "));
        if label {
            if case["command"] == "set" {
                args.extend(["-n".into(), case["label_name"].as_str().unwrap().into()]);
            }
        } else {
            args.extend(["-I".into(), case["identity"].as_str().unwrap().into()]);
            if case["command"] == "set" {
                args.extend(["-R".into(), case["role"].as_str().unwrap().into()]);
            }
        }
        if case["force"] == true {
            args.push("-f".into());
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
        let (api, wire, credential) = setup(replies);
        let targets = target(label.then_some("Notebook"));
        let definitions = label.then(|| Definitions::new(&case["definitions"]["labels"]));
        let mut prompt = Prompt {
            answers: case["confirmations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_bool().unwrap())
                .collect(),
            questions: Vec::new(),
        };
        let result = {
            let mut host = host(&api, &targets);
            host.label_definitions = if case["configured"] == false {
                None
            } else {
                definitions.as_ref().map(|d| d.0.as_path())
            };
            host.confirmation = Some(&mut prompt);
            host.execute(&command, pending()).await
        };
        let expected = &case["expected"];
        assert_eq!(
            json!(prompt.questions),
            expected["questions"],
            "{}",
            case["name"]
        );
        let diagnostics: Vec<_> = expected["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| {
                line.as_str()
                    .unwrap()
                    .replace("'Resource'", "'Workspace.Workspace'")
            })
            .collect();
        let actual_requests: Vec<_> = wire.requests.lock().unwrap().iter().map(|r| {
            let bytes = azure_core::Bytes::from(r.body());
            json!({"method":r.method().to_string(), "url":r.url().as_str(),
                "body":if bytes.is_empty() { Value::Null } else { serde_json::from_slice::<Value>(&bytes).unwrap() }})
        }).collect();
        assert_eq!(
            json!(actual_requests),
            expected["requests"],
            "{}",
            case["name"]
        );
        assert_eq!(
            credential.calls.load(Ordering::SeqCst),
            actual_requests.len()
        );
        if expected["error"].is_null() {
            let value = result.as_ref().unwrap();
            assert_eq!(
                json!(value.message),
                expected["message"],
                "{}",
                case["name"]
            );
            assert_eq!(value.diagnostics, diagnostics, "{}", case["name"]);
            assert_eq!(
                json!(value.warnings),
                expected["warnings"],
                "{}",
                case["name"]
            );
            for format in [OutputFormat::Json, OutputFormat::Text] {
                let source = streams["cases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| {
                        row["kind"] == "label-remove"
                            && row["format"]
                                == if format == OutputFormat::Json {
                                    "json"
                                } else {
                                    "text"
                                }
                    })
                    .unwrap();
                let expected_stdout = expected["message"]
                    .as_str()
                    .map(|message| {
                        source["expected"]["stdout"]
                            .as_str()
                            .unwrap()
                            .replace("Label removed", message)
                            .replace("\"label\"", &format!("\"{family}\""))
                    })
                    .unwrap_or_default();
                let output = render(
                    &command,
                    result.clone(),
                    Timestamp::parse("2026-01-02T03:04:05.000006Z").unwrap(),
                    &FamilyPresentation::new(format, FamilySurface::Get),
                    &ascii_width,
                )
                .unwrap();
                let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                assert_eq!(
                    output.write_to(&mut stdout, &mut stderr).unwrap(),
                    0,
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    String::from_utf8(stdout).unwrap(),
                    expected_stdout,
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    String::from_utf8(stderr).unwrap(),
                    diagnostics
                        .iter()
                        .map(|line| format!("{line}\n"))
                        .collect::<String>(),
                    "{}",
                    case["name"]
                );
                assert_eq!(json!(output.warnings), expected["warnings"]);
            }
        } else {
            let error = result.as_ref().unwrap_err();
            assert_eq!(
                error.code,
                expected["error"]["code"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                error.message,
                expected["error"]["message"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(error.diagnostics, diagnostics);
            let output = render(
                &command,
                result,
                Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                &FamilyPresentation::new(OutputFormat::Json, FamilySurface::Mutation),
                &ascii_width,
            )
            .unwrap();
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 1);
            assert_eq!(
                serde_json::from_slice::<Value>(&stdout).unwrap(),
                json!({
                    "timestamp":"2000-01-01T00:00:00Z",
                    "status":"Failure",
                    "command":family,
                    "result":{
                        "error_code":expected["error"]["code"],
                        "message":expected["error"]["message"],
                    },
                }),
                "{}",
                case["name"]
            );
            assert_eq!(
                String::from_utf8(stderr).unwrap(),
                diagnostics
                    .iter()
                    .map(|line| format!("{line}\n"))
                    .collect::<String>(),
                "{}",
                case["name"],
            );
        }
    }
    assert_eq!(leaves.len(), 4);
}

#[tokio::test]
async fn unconfigured_confirmation_unsupported_resource_and_uncertain_mutations_fail_without_replay()
 {
    let (api, wire, credential) = setup(vec![]);
    let targets = target(None);
    let mut host = host(&api, &targets);
    let command = parse(&[
        "acl",
        "set",
        "Workspace.Workspace",
        "-I",
        "principal",
        "-R",
        "Viewer",
    ]);
    assert_eq!(
        host.execute(&command, pending()).await.unwrap_err().code,
        "NotConfigured"
    );
    let command = parse(&["label", "rm", "Workspace.Workspace", "-f"]);
    assert_eq!(
        host.execute(&command, pending()).await.unwrap_err().code,
        "UnsupportedCommand"
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());

    for status in [403, 500, 202] {
        let (api, wire, _) = setup(vec![
            reply(200, "{\"value\":[]}", &[]),
            reply(status, "{}", &[]),
        ]);
        let targets = target(None);
        let command = parse(&[
            "acl",
            "set",
            "Workspace.Workspace",
            "-I",
            "principal",
            "-R",
            "Viewer",
            "-f",
        ]);
        let error = super::host(&api, &targets)
            .execute(&command, pending())
            .await
            .unwrap_err();
        assert_eq!(wire.requests.lock().unwrap().len(), 2);
        if status == 202 {
            assert!(error.mutation_may_have_occurred);
        }
    }
    let (api, wire, _) = setup(vec![]);
    let targets = target(Some("Notebook"));
    let command = parse(&[
        "acl",
        "rm",
        "Workspace.Workspace/Item.Notebook",
        "-I",
        "principal",
        "-f",
    ]);
    assert_eq!(
        super::host(&api, &targets)
            .execute(&command, pending())
            .await
            .unwrap_err()
            .code,
        "UnsupportedCommand"
    );
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn every_headerless_accepted_mutation_retains_uncertainty_and_does_not_replay() {
    let assignment = json!({"value":[{
        "id":"assignment",
        "role":"Viewer",
        "principal":{"id":"principal","displayName":"Principal","type":"User"},
    }]});
    let definitions = Definitions::new(&json!([{"name":"Public","id":"label"}]));
    for (args, listed, method, url) in [
        (
            vec![
                "acl",
                "set",
                "Workspace.Workspace",
                "-I",
                "principal",
                "-R",
                "Viewer",
                "-f",
            ],
            Some(json!({"value":[]})),
            "POST",
            "https://api.fabric.microsoft.com/v1/workspaces/resource/roleAssignments",
        ),
        (
            vec![
                "acl",
                "set",
                "Workspace.Workspace",
                "-I",
                "principal",
                "-R",
                "Viewer",
                "-f",
            ],
            Some(assignment.clone()),
            "PATCH",
            "https://api.fabric.microsoft.com/v1/workspaces/resource/roleAssignments/principal",
        ),
        (
            vec!["acl", "rm", "Workspace.Workspace", "-I", "principal", "-f"],
            Some(assignment),
            "DELETE",
            "https://api.fabric.microsoft.com/v1/workspaces/resource/roleAssignments/assignment",
        ),
        (
            vec![
                "label",
                "set",
                "Workspace.Workspace/Item.Notebook",
                "-n",
                "Public",
                "-f",
            ],
            None,
            "POST",
            "https://api.fabric.microsoft.com/v1/admin/items/bulkSetLabels",
        ),
        (
            vec!["label", "rm", "Workspace.Workspace/Item.Notebook", "-f"],
            None,
            "POST",
            "https://api.fabric.microsoft.com/v1/admin/items/bulkRemoveLabels",
        ),
    ] {
        let label = args[0] == "label";
        let mut replies = Vec::new();
        if let Some(listed) = &listed {
            replies.push(reply(200, &listed.to_string(), &[]));
        }
        replies.push(reply(202, "{}", &[]));
        let (api, wire, credential) = setup(replies);
        let targets = target(label.then_some("Notebook"));
        let mut host = host(&api, &targets);
        host.label_definitions = Some(&definitions.0);
        let command = parse(&args);
        let error = host.execute(&command, pending()).await.unwrap_err();
        assert!(error.mutation_may_have_occurred, "{method} {url}");
        assert_eq!(error.code, "UnexpectedError");
        assert_eq!(error.message, "unsupported completion status 202");
        assert_eq!(error.warnings.len(), usize::from(method == "PATCH"));
        assert_eq!(error.diagnostics.len(), usize::from(method != "PATCH"));
        let requests = wire.requests.lock().unwrap();
        let count = usize::from(listed.is_some()) + 1;
        assert_eq!(requests.len(), count);
        assert_eq!(requests.last().unwrap().method().to_string(), method);
        assert_eq!(requests.last().unwrap().url().as_str(), url);
        assert_eq!(credential.calls.load(Ordering::SeqCst), count);
    }
}
