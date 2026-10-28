// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root argv -> FamilySession -> actual shared HTTP, file worker and OneLake SDK.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

#[path = "family_support/deferred_export.rs"]
mod deferred_export;
#[path = "family_support/deploy_paths.rs"]
mod deploy_paths;
#[path = "family_support/metadata_snapshot.rs"]
mod family_metadata;
mod family_support;
#[path = "export_support/import_input.rs"]
mod import_input;

use fabric_cli::families::FamilyEvidence;
use fabric_cli_local::ConfigStore;
use fabric_cli_output::CommandError;
use fabric_cli_transfer::{
    TransferEvent, TransferFailure, TransferFiles, TransferFuture, TransferUi,
};
use family_support::{Harness, success};
use serde_json::{Value, json};
use std::{
    fs,
    future::pending,
    sync::{Arc, Mutex, atomic::Ordering},
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const NOTEBOOK: &str = "22222222-2222-2222-2222-222222222222";
const PIPELINE: &str = "33333333-3333-3333-3333-333333333333";
const LAKE: &str = "66666666-6666-6666-6666-666666666666";
const OPERATION: &str = "77777777-7777-7777-7777-777777777777";

fn host() -> Harness {
    let host = Harness::new();
    fs::create_dir(host.root.join("output")).unwrap();
    host
}

fn items() -> Value {
    json!([
        {"id":NOTEBOOK,"displayName":"zulu","type":"Notebook"},
        {"id":PIPELINE,"displayName":"alpha","type":"DataPipeline"},
    ])
}

fn definition(path: &str, payload: &str) -> Value {
    json!({"definition":{"parts":[{"path":path,"payload":payload,"payloadType":"InlineBase64"}]}})
}

fn failed(output: &fabric_cli::families::FamilyOutput) {
    assert_ne!(output.plan.exit_code, 0, "{:?}", output.evidence);
    assert!(matches!(output.evidence, FamilyEvidence::Transfer(Err(_))));
}

#[derive(Default)]
struct Ui(Mutex<Vec<TransferEvent>>);
impl TransferUi for Ui {
    fn emit(&self, event: TransferEvent) -> Result<(), CommandError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
    fn confirm<'a>(&'a self, _: &'a str) -> TransferFuture<'a, Result<bool, CommandError>> {
        Box::pin(async { panic!("--force must not prompt") })
    }
    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>> {
        Box::pin(async { panic!("--all must not invoke selection UI") })
    }
}

#[tokio::test]
async fn workspace_all_displays_configuration_sort_but_executes_cached_source_order() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/definition_export_discovery_source.json"
    ))
    .unwrap();
    let mut host = host();
    let store = ConfigStore::new(&host.root);
    let mut settings = store.read().unwrap();
    settings.insert("output_item_sort_criteria".into(), json!("bytype"));
    store.write(&settings).unwrap();
    host.metadata_with_id(WS, items());
    host.reply(200, json!({"id":NOTEBOOK}));
    host.reply(
        200,
        definition(
            "notebook-content.ipynb",
            "eyJjZWxscyI6W3sib3V0cHV0cyI6WzFdfV19",
        ),
    );
    host.reply(200, json!({"id":PIPELINE}));
    host.reply(200, definition("pipeline-content.json", "e30="));
    let ui = Ui::default();
    let output = host
        .execute_with_capabilities(
            &[
                "export",
                "Demo.Workspace",
                "--all",
                "--force",
                "-o",
                "output",
            ],
            pending(),
            &ui,
        )
        .await;
    success(&output);
    let selected = receipt["workspace_selection"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        ui.0.lock()
            .unwrap()
            .contains(&TransferEvent::Grey(selected))
    );
    assert!(ui.0.lock().unwrap().contains(&TransferEvent::Grey(
        "Exporting 'Demo.Workspace/zulu.Notebook' \u{2192} 'output/zulu.Notebook'...".into()
    )));
    let requests = host.requests();
    assert!(requests[2]["url"].as_str().unwrap().ends_with(NOTEBOOK));
    assert!(requests[4]["url"].as_str().unwrap().ends_with(PIPELINE));
    let file: Value = serde_json::from_slice(
        &fs::read(
            host.root
                .join("output")
                .join("zulu.Notebook")
                .join("notebook-content.ipynb"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(file["cells"][0]["outputs"], json!([]));
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert_eq!(
        host.requests().len(),
        6,
        "writer failure cannot replay definitions"
    );
}

#[tokio::test]
async fn item_formats_use_source_defaults_or_exact_explicit_selector() {
    for (kind, selector, format) in [
        ("Notebook", None, Some("ipynb")),
        ("Notebook", Some(".py"), Some("fabricGitSource")),
        ("SparkJobDefinition", None, Some("SparkJobDefinitionV1")),
        (
            "SparkJobDefinition",
            Some("SparkJobDefinitionV2"),
            Some("SparkJobDefinitionV2"),
        ),
        ("DataPipeline", None, None),
        ("SemanticModel", Some("TMDL"), Some("TMDL")),
    ] {
        let mut host = host();
        host.metadata_with_id(
            WS,
            json!([{"id":NOTEBOOK,"displayName":"Book","type":kind}]),
        );
        host.reply(200, json!({"id":NOTEBOOK}));
        let (part, payload) = if kind == "Notebook" {
            if selector == Some(".py") {
                ("notebook-content.py", "cHJpbnQoMSk=")
            } else {
                (
                    "notebook-content.ipynb",
                    "eyJjZWxscyI6W3sib3V0cHV0cyI6WzFdfV19",
                )
            }
        } else {
            ("part.bin", "/w0KAA==")
        };
        host.reply(200, definition(part, payload));
        let target = format!("Demo.Workspace/Book.{kind}");
        let mut args = vec!["export", &target, "--force", "-o", "output"];
        if let Some(selector) = selector {
            args.extend(["--format", selector]);
        }
        success(&host.execute(&args).await);
        let requests = host.requests();
        let expected = format.map_or_else(
            || "/getDefinition".into(),
            |format| format!("/getDefinition?format={format}"),
        );
        assert!(requests[3]["url"].as_str().unwrap().ends_with(&expected));
        assert_eq!(requests[3]["raw_body"], "");
        let bytes = fs::read(
            host.root
                .join("output")
                .join(format!("Book.{kind}"))
                .join(part),
        )
        .unwrap();
        if kind == "Notebook" && selector != Some(".py") {
            let notebook: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(notebook["cells"][0]["outputs"], json!([]));
        } else if kind == "Notebook" {
            assert_eq!(bytes, b"print(1)");
        } else {
            assert_eq!(bytes, [255, 13, 10, 0]);
        }
    }
}

#[tokio::test]
async fn malformed_suffix_part_retains_encoded_source_payload_and_later_files() {
    let mut host = host();
    host.metadata_with_id(
        WS,
        json!([{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]),
    );
    host.reply(200, json!({"id":PIPELINE}));
    host.reply(
        200,
        json!({"definition":{"parts":[
            {"path":"first.bin","payload":"/w=="},
            {"path":"bad.json","payload":"e2Jyb2tlbg=="},
            {"path":"last.txt","payload":"eA=="},
        ]}}),
    );
    let output = host
        .execute(&[
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-o",
            "output",
            "-f",
        ])
        .await;
    success(&output);
    let root = host.root.join("output").join("Book.DataPipeline");
    assert_eq!(fs::read(root.join("first.bin")).unwrap(), [255]);
    assert_eq!(fs::read(root.join("bad.json")).unwrap(), b"e2Jyb2tlbg==");
    assert_eq!(fs::read(root.join("last.txt")).unwrap(), b"x");
}

#[tokio::test]
async fn nested_folder_bulk_selective_uses_source_folder_first_order_and_retains_outputs() {
    let mut host = host();
    let folders = json!({"value":[
        {"id":"outer","displayName":"Outer"},
        {"id":"selected","displayName":"Selected","parentFolderId":"outer"},
        {"id":"child","displayName":"Child","parentFolderId":"selected"},
    ]});
    host.reply(
        200,
        json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
    );
    host.reply(200, folders);
    host.reply(
        200,
        json!({"value":[
            {"id":NOTEBOOK,"displayName":"zulu","type":"Notebook","folderId":"selected"},
            {"id":PIPELINE,"displayName":"alpha","type":"DataPipeline","folderId":"child"},
            {"id":"unknown-future","displayName":"Ignore","type":"FutureType"},
        ]}),
    );
    host.reply(200, json!({"definitionParts":[
        {"path":"/Outer/Selected/zulu.Notebook/notebook-content.ipynb","payload":"eyJjZWxscyI6W3sib3V0cHV0cyI6WzFdfV19"},
        {"path":"/Outer/Selected/Child/alpha.DataPipeline/pipeline-content.json","payload":"e30="},
    ]}));
    let output = host
        .execute(&[
            "bulk-export",
            "Demo.Workspace/Outer.Folder/Selected.Folder",
            "--recursive",
            "-f",
            "-o",
            "output",
        ])
        .await;
    success(&output);
    assert_eq!(
        host.requests()[3]["body"],
        json!({"items":[{"id":PIPELINE},{"id":NOTEBOOK}],"mode":"Selective"})
    );
    assert!(
        matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
        if result.exported_ids == [PIPELINE, NOTEBOOK])
    );
    let notebook: Value = serde_json::from_slice(
        &fs::read(
            host.root
                .join("output")
                .join("Selected")
                .join("zulu.Notebook")
                .join("notebook-content.ipynb"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(notebook["cells"][0]["outputs"], json!([1]));
    assert!(
        host.root
            .join("output")
            .join("Selected")
            .join("Child")
            .join("alpha.DataPipeline")
            .join("pipeline-content.json")
            .is_file()
    );
}

#[tokio::test]
async fn workspace_bulk_response_index_order_missing_and_unknown_ids_drive_summary() {
    let mut host = host();
    host.metadata_with_id(WS, items());
    host.reply(200, json!({
        "definitionParts":[
            {"path":"/alpha.DataPipeline/part.txt","payload":"eA=="},
            {"path":"/unknown.DataPipeline/part.txt","payload":"eQ=="},
        ],
        "itemDefinitionsIndex":[{"id":"unknown","rootPath":"/unknown.DataPipeline"},{"id":PIPELINE,"rootPath":"/alpha.DataPipeline"}],
    }));
    let output = host
        .execute(&[
            "bulk-export",
            "Demo.Workspace",
            "--recursive",
            "-f",
            "-o",
            "output",
        ])
        .await;
    let data: Value = serde_json::from_str(&success(&output)).unwrap();
    assert_eq!(data["result"]["data"][0]["exported"], 1);
    assert_eq!(data["result"]["data"][0]["skipped"], 1);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
        if result.exported_ids == [PIPELINE] && result.skipped_ids == [NOTEBOOK])
    );
    assert_eq!(
        host.requests()[2]["raw_body"],
        "{\"items\": [], \"mode\": \"All\"}"
    );
    assert!(
        !host
            .root
            .join("output")
            .join("unknown.DataPipeline")
            .exists()
    );
}

#[tokio::test]
async fn later_item_page_failure_never_exports_a_partial_selection() {
    let mut host = host();
    host.reply(
        200,
        json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
    );
    host.reply(200, json!({"value":[{"id":NOTEBOOK,"displayName":"Book","type":"Notebook"}],"continuationToken":"next+/="}));
    host.reply(
        403,
        json!({"errorCode":"Denied","message":"later page denied"}),
    );
    let output = host
        .execute(&["export", "Demo.Workspace", "--all", "-f", "-o", "output"])
        .await;
    failed(&output);
    assert!(
        host.requests()[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("continuationToken=next%2B%2F%3D")
    );
    assert!(
        fs::read_dir(host.root.join("output"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn bulk_accepted_poll_result_is_written_once_and_writer_failure_does_not_replay() {
    let mut host = host();
    host.metadata_with_id(WS, items());
    let poll = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let result = format!("{poll}/result");
    host.headers(
        202,
        Value::Null,
        &[
            ("Location", &poll),
            ("x-ms-operation-id", OPERATION),
            ("Retry-After", "0"),
        ],
    );
    host.headers(200, json!({"status":"Succeeded"}), &[("Location", &result)]);
    host.reply(
        200,
        json!({
            "definitionParts":[{"path":"/alpha.DataPipeline/part.txt","payload":"eA=="}],
            "itemDefinitionsIndex":[{"id":PIPELINE,"rootPath":"/alpha.DataPipeline"}],
        }),
    );
    let ui = Ui::default();
    let output = host
        .execute_with_capabilities(
            &[
                "bulk-export",
                "Demo.Workspace",
                "--recursive",
                "-f",
                "-o",
                "output",
            ],
            pending(),
            &ui,
        )
        .await;
    success(&output);
    assert_eq!(
        fs::read(
            host.root
                .join("output")
                .join("alpha.DataPipeline")
                .join("part.txt")
        )
        .unwrap(),
        b"x"
    );
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert_eq!(
        ui.0.lock().unwrap()[0],
        TransferEvent::Warning(
            "Item definitions are exported without their sensitivity labels".into()
        )
    );
    assert_eq!(host.requests().len(), 5);
    assert_eq!(host.requests()[3]["method"], "GET");
    assert!(
        host.requests()[4]["url"]
            .as_str()
            .unwrap()
            .ends_with("/result")
    );
}

#[tokio::test]
async fn bulk_precondition_warnings_precede_discovery_failure_and_preserve_existing_files() {
    let mut host = host();
    fs::write(host.root.join("output").join("existing.txt"), b"keep").unwrap();
    host.reply(
        200,
        json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
    );
    host.reply(
        403,
        json!({"errorCode":"Denied","message":"discovery denied"}),
    );
    let ui = Ui::default();
    let output = host
        .execute_with_capabilities(
            &[
                "bulk-export",
                "Demo.Workspace",
                "--recursive",
                "-f",
                "-o",
                "output",
            ],
            pending(),
            &ui,
        )
        .await;
    failed(&output);
    assert_eq!(
        *ui.0.lock().unwrap(),
        vec![
            TransferEvent::Warning(
                "Item definitions are exported without their sensitivity labels".into()
            ),
            TransferEvent::Warning("Exporting to a non-empty output folder".into()),
        ]
    );
    assert_eq!(host.requests().len(), 2);
    assert_eq!(
        fs::read(host.root.join("output").join("existing.txt")).unwrap(),
        b"keep"
    );
}

fn remote_metadata(host: &Harness, definition: Value) {
    let listing = json!([
        {"id":NOTEBOOK,"displayName":"Book","type":"DataPipeline"},
        {"id":LAKE,"displayName":"Lake","type":"Lakehouse"},
    ]);
    host.metadata_with_id(WS, listing.clone());
    host.reply(200, json!({"id":NOTEBOOK}));
    host.reply(200, definition);
    host.metadata_with_id(WS, listing);
}

#[tokio::test]
async fn remote_lakehouse_uses_existing_storage_sdk_exact_json_and_no_local_files() {
    let mut host = host();
    remote_metadata(
        &host,
        definition("object.json", "eyJ6Ijoiw6kiLCJhIjpbMSx0cnVlXX0="),
    );
    host.reply(201, Value::Null);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let output = host
        .execute(&[
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files",
        ])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 9);
    assert!(
        matches!(&output.evidence, FamilyEvidence::Transfer(Ok(result))
        if result.exported_ids == [NOTEBOOK])
    );
    assert!(requests[6]["url"].as_str().unwrap().contains(&format!(
        "/{WS}/{LAKE}/Files/Book.DataPipeline/object.json/?resource=file"
    )));
    assert_eq!(
        requests[7]["raw_body"],
        "{\"z\": \"\\u00e9\", \"a\": [1, true]}"
    );
    assert!(
        requests[8]["url"]
            .as_str()
            .unwrap()
            .ends_with("action=flush&position=31")
    );
    assert!(
        host.credential
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|scopes| scopes == &[fabric_sdk::Audience::OneLake.scope()])
    );
    assert!(!host.root.join("Demo.Workspace").exists());
}

#[tokio::test]
async fn remote_empty_payload_omits_append_and_flushes_zero() {
    let mut host = host();
    remote_metadata(&host, definition("empty.txt", ""));
    host.reply(201, Value::Null);
    host.reply(200, Value::Null);
    let output = host
        .execute(&[
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files",
        ])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 8);
    assert!(
        requests[7]["url"]
            .as_str()
            .unwrap()
            .ends_with("action=flush&position=0")
    );
}

#[tokio::test]
async fn remote_unicode_text_retains_actual_python_character_count_flush() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/definition_export_unicode_source.json"
    ))
    .unwrap();
    let mut host = host();
    remote_metadata(
        &host,
        definition(
            "unicode.txt",
            receipt["calls"][1]["base64"].as_str().unwrap(),
        ),
    );
    host.reply(201, Value::Null);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let output = host
        .execute(&[
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files",
        ])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests[7]["raw_body"], "\u{e9}\u{1f600}");
    assert!(requests[8]["url"].as_str().unwrap().ends_with(&format!(
        "action=flush&position={}",
        receipt["calls"][2]["position"].as_u64().unwrap(),
    )));
}

#[tokio::test]
async fn bulk_duplicate_ids_use_last_index_object_but_original_missing_objects() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/definition_export_index_source.json"
    ))
    .unwrap();
    for case in receipt["cases"].as_array().unwrap() {
        let mut host = host();
        host.metadata_with_id(
            WS,
            json!([
                {"id":NOTEBOOK,"displayName":"Book","type":"Notebook"},
                {"id":NOTEBOOK,"displayName":"Pipeline","type":"DataPipeline"},
                {"id":PIPELINE,"displayName":"Model","type":"SemanticModel"},
            ]),
        );
        let mut index = vec![json!({"id":PIPELINE,"rootPath":"/Model.SemanticModel"})];
        if !case["missing_duplicate"].as_bool().unwrap() {
            index.insert(
                0,
                json!({"id":NOTEBOOK,"rootPath":"/Pipeline.DataPipeline"}),
            );
        }
        host.reply(
            200,
            json!({"definitionParts":[],"itemDefinitionsIndex":index}),
        );
        let output = host
            .execute(&[
                "bulk-export",
                "Demo.Workspace",
                "--recursive",
                "-f",
                "-o",
                "output",
            ])
            .await;
        let data: Value = serde_json::from_str(&success(&output)).unwrap();
        assert_eq!(
            data["result"]["data"][0]["exported_types"],
            case["exported_types"]
        );
        assert_eq!(
            data["result"]["data"][0]["skipped_types"],
            case["skipped_types"]
        );
    }
}

#[tokio::test]
async fn remote_second_upload_failure_retains_first_file_and_never_replays_create() {
    let mut host = host();
    remote_metadata(
        &host,
        json!({"definition":{"parts":[
            {"path":"first.txt","payload":"eA=="},{"path":"second.txt","payload":"eQ=="},
        ]}}),
    );
    host.reply(201, Value::Null);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    host.reply(201, Value::Null);
    host.reply(403, json!({"error":{"code":"AuthorizationFailure"}}));
    let output = host
        .execute(&[
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files",
        ])
        .await;
    failed(&output);
    let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
        unreachable!()
    };
    assert_eq!(error.completed_paths, ["Book.DataPipeline/first.txt"]);
    let TransferFailure::Definition(error) = &error.failure else {
        unreachable!()
    };
    assert_eq!(
        error.incomplete_path.as_deref(),
        Some("Book.DataPipeline/second.txt")
    );
    let fabric_deploy::definitions::DefinitionErrorKind::FileSystem(error) = &error.kind else {
        unreachable!()
    };
    let storage = error
        .get_ref()
        .unwrap()
        .downcast_ref::<fabric_sdk_onelake::OneLakeMutationError>()
        .unwrap();
    assert!(storage.mutation_may_have_occurred);
    assert_eq!(
        storage.completed_steps,
        [fabric_sdk_onelake::OneLakeWriteStep::CreateFile]
    );
    assert_eq!(
        host.requests()
            .iter()
            .filter(|request| request["method"] == "PUT")
            .count(),
        2
    );
}

#[tokio::test]
async fn remote_bare_accepted_create_is_incomplete_not_export_success() {
    let mut host = host();
    remote_metadata(&host, definition("part.txt", "eA=="));
    host.reply(202, Value::Null);
    let output = host
        .execute(&[
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files",
        ])
        .await;
    failed(&output);
    let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
        unreachable!()
    };
    assert!(error.completed_paths.is_empty());
    assert!(error.exported_ids.is_empty());
    assert_eq!(host.requests().len(), 7);
    assert_eq!(host.requests()[6]["method"], "PUT");
}

#[tokio::test]
async fn remote_auth_failure_cannot_fall_back_to_coincidentally_named_local_directory() {
    let mut host = host();
    let coincidental = host
        .root
        .join("Demo.Workspace")
        .join("Lake.Lakehouse")
        .join("Files");
    fs::create_dir_all(&coincidental).unwrap();
    host.metadata_with_id(
        WS,
        json!([{"id":NOTEBOOK,"displayName":"Book","type":"DataPipeline"}]),
    );
    host.reply(200, json!({"id":NOTEBOOK}));
    host.reply(200, definition("part.txt", "eA=="));
    host.reply(
        403,
        json!({"errorCode":"Denied","message":"destination discovery denied"}),
    );
    let output = host
        .execute(&[
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files",
        ])
        .await;
    failed(&output);
    assert!(fs::read_dir(coincidental).unwrap().next().is_none());
    assert_eq!(host.requests().len(), 5);
}

struct InjectedFiles {
    local: fabric_cli_transfer::LocalTransferFiles,
    writes: usize,
    cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl fabric_deploy::definitions::DefinitionFileSystem for InjectedFiles {
    fn preflight(
        &mut self,
        root: &std::path::Path,
        files: &[fabric_deploy::definitions::DefinitionFile],
    ) -> std::io::Result<()> {
        self.local.preflight(root, files)
    }
    fn create_directory(&mut self, root: &std::path::Path, name: &str) -> std::io::Result<()> {
        self.local.create_directory(root, name)
    }
    fn write(
        &mut self,
        root: &std::path::Path,
        file: &fabric_deploy::definitions::DefinitionFile,
    ) -> std::io::Result<()> {
        if self.writes == 1 && self.cancelled.is_none() {
            fs::write(root.join(&file.path), &file.bytes[..1])?;
            return Err(std::io::Error::other("injected partial write"));
        }
        self.local.write(root, file)?;
        self.writes += 1;
        if let Some(cancelled) = &self.cancelled {
            cancelled.store(true, Ordering::Release);
        }
        Ok(())
    }
}

impl fabric_deploy::definitions::DefinitionImportFileSystem for InjectedFiles {
    fn entries(
        &mut self,
        root: &std::path::Path,
        relative: &str,
    ) -> std::io::Result<Vec<fabric_deploy::definitions::DefinitionEntry>> {
        self.local.entries(root, relative)
    }
    fn read(
        &mut self,
        root: &std::path::Path,
        relative: &str,
        max_bytes: usize,
    ) -> std::io::Result<Vec<u8>> {
        self.local.read(root, relative, max_bytes)
    }
}

impl fabric_cli_transfer::TransferFiles for InjectedFiles {
    fn resolve_directory(&mut self, input: &str) -> std::io::Result<std::path::PathBuf> {
        self.local.resolve_directory(input)
    }
    fn is_empty(&mut self, root: &std::path::Path) -> std::io::Result<bool> {
        self.local.is_empty(root)
    }
}

struct Timer;
impl fabric_sdk_http::ExecutionTimer for Timer {
    fn sleep(&self, delay: std::time::Duration) -> fabric_sdk_http::ExecutionFuture<'_, ()> {
        if delay == std::time::Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
    }
}

struct MutationUi;
impl fabric_cli_mutations::MutationUi for MutationUi {
    fn notice(&mut self, _: fabric_cli_mutations::NoticeKind, _: &str) {}
    fn confirm<'a>(&'a mut self, _: &'a str) -> fabric_cli_mutations::CommandFuture<'a, bool> {
        Box::pin(async { panic!("forced export cannot ask mutation UI") })
    }
}

async fn injected_export(
    host: &mut Harness,
    files: &mut dyn fabric_cli_transfer::TransferFiles,
    cancelled: impl std::future::Future<Output = ()> + Send,
) -> fabric_cli::families::FamilyOutput {
    let client = host.http.clone();
    injected_export_with_http(
        host,
        files,
        cancelled,
        &[
            "fab-rs",
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "output",
        ],
        client,
    )
    .await
}

async fn injected_export_with_http(
    host: &mut Harness,
    files: &mut dyn fabric_cli_transfer::TransferFiles,
    cancelled: impl std::future::Future<Output = ()> + Send,
    args: &[&str],
    client: Arc<dyn azure_core::http::HttpClient>,
) -> fabric_cli::families::FamilyOutput {
    injected_export_at_endpoint(
        host,
        files,
        cancelled,
        args,
        client,
        fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric),
    )
    .await
}

async fn injected_export_at_endpoint(
    host: &mut Harness,
    files: &mut dyn fabric_cli_transfer::TransferFiles,
    cancelled: impl std::future::Future<Output = ()> + Send,
    args: &[&str],
    client: Arc<dyn azure_core::http::HttpClient>,
    endpoint: fabric_sdk::EndpointBinding,
) -> fabric_cli::families::FamilyOutput {
    let sdk = fabric_sdk::Client::with_context(fabric_sdk::RequestContext::with_endpoint(
        host.credential.clone(),
        fabric_sdk::CredentialSelection::new(
            "offline-account",
            "tenant",
            fabric_sdk::Audience::Fabric,
        )
        .unwrap(),
        endpoint.clone(),
    ));
    let http = fabric_sdk_http::HttpExecutor::new(client, Arc::new(Timer), Default::default())
        .unwrap()
        .with_endpoint(endpoint);
    let tenant = fabric_cli_context::Element::tenant("Tenant", "tenant");
    let reads = fabric_cli::session::ReadSession {
        sdk: &sdk,
        executor: &http,
        tenant: tenant.clone(),
        current_path: "/",
        home: fabric_cli_context::HomeContext::Unspecified,
        timestamp: fabric_cli_output::Timestamp::parse("2026-10-28T12:00:00Z").unwrap(),
        config: Default::default(),
    };
    let store = ConfigStore::new(&host.root);
    let mut navigation = fabric_cli_local::NavigationState::new(
        fabric_cli_context::PathElement::from_fabric(tenant),
        fabric_cli_context::HomeContext::Unspecified,
        host.root.to_string_lossy().into_owned(),
        37,
        true,
    )
    .unwrap();
    let invocation = fabric_cli::parser::parse_args(args.iter().copied()).unwrap();
    let output = fabric_cli::families::FamilySession {
        reads: &reads,
        http: &http,
        store: &store,
        navigation: &mut navigation,
        local: &mut host.effects,
        mutations: &mut MutationUi,
        confirmation: None,
        transfer_ui: &Ui::default(),
        transfer_files: Some(files),
        resource_files: None,
    }
    .dispatch(&invocation, cancelled)
    .await
    .unwrap();
    assert!(host.http.replies.lock().unwrap().is_empty());
    output
}

#[tokio::test]
async fn root_partial_file_write_and_cancellation_keep_exact_prior_progress() {
    for cancel in [false, true] {
        let mut host = host();
        host.metadata_with_id(
            WS,
            json!([{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]),
        );
        host.reply(200, json!({"id":PIPELINE}));
        host.reply(200, json!({"definition":{"parts":[
            {"path":"first.txt","payload":"Zmlyc3Q="},{"path":"second.txt","payload":"c2Vjb25k"},
        ]}}));
        let signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut files = InjectedFiles {
            local: fabric_cli_transfer::LocalTransferFiles::new(
                host.root.clone(),
                None,
                Default::default(),
            )
            .unwrap(),
            writes: 0,
            cancelled: cancel.then(|| signal.clone()),
        };
        let output = injected_export(
            &mut host,
            &mut files,
            std::future::poll_fn(|_| {
                if signal.load(Ordering::Acquire) {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }),
        )
        .await;
        failed(&output);
        let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
            unreachable!()
        };
        assert_eq!(error.completed_paths, ["Book.DataPipeline/first.txt"]);
        let TransferFailure::Definition(error) = &error.failure else {
            unreachable!()
        };
        assert_eq!(
            error.incomplete_path.as_deref(),
            (!cancel).then_some("Book.DataPipeline/second.txt")
        );
        let root = host.root.join("output").join("Book.DataPipeline");
        assert_eq!(fs::read(root.join("first.txt")).unwrap(), b"first");
        if cancel {
            assert!(!root.join("second.txt").exists());
        } else {
            assert_eq!(fs::read(root.join("second.txt")).unwrap(), b"s");
        }
        assert_eq!(host.requests().len(), 4);
    }
}

#[tokio::test]
async fn root_cancelled_before_discovery_has_no_http_or_filesystem_effect() {
    let mut host = host();
    let output = host
        .execute_with_cancelled(
            &["export", "Demo.Workspace", "--all", "-f", "-o", "output"],
            std::future::ready(()),
        )
        .await;
    assert_ne!(output.plan.exit_code, 0);
    assert!(matches!(output.evidence, FamilyEvidence::Local));
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_ne!(output.write_to(&mut stdout, &mut stderr).unwrap(), 0);
    let envelope: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(envelope["result"]["error_code"], "Cancelled");
    assert!(stderr.is_empty());
    assert!(host.requests().is_empty());
    assert!(
        fs::read_dir(host.root.join("output"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn caller_selected_export_root_outside_current_root_keeps_async_confinement() {
    let mut host = host();
    let selected = host.root.with_file_name(format!(
        "{}-selected",
        host.root.file_name().unwrap().to_string_lossy(),
    ));
    fs::create_dir(&selected).unwrap();
    let selected = fs::canonicalize(selected).unwrap();
    host.metadata_with_id(
        WS,
        json!([{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]),
    );
    host.reply(200, json!({"id":PIPELINE}));
    host.reply(200, definition("part.txt", "eA=="));
    let mut files = fabric_cli::families::transfer_files::TokioTransferFiles::for_export_root(
        selected.clone(),
        "output".into(),
        Default::default(),
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    let output = injected_export(&mut host, &mut files, pending()).await;
    success(&output);
    assert_eq!(
        fs::read(selected.join("Book.DataPipeline").join("part.txt")).unwrap(),
        b"x"
    );
    assert!(
        fs::read_dir(host.root.join("output"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(files.resolve_export_directory_async("..").await.is_err());
    fs::remove_dir_all(selected).unwrap();
}

#[derive(Debug)]
struct BinaryHttp {
    base: Arc<family_support::Http>,
    bytes: Mutex<Vec<Vec<u8>>>,
}

#[async_trait::async_trait]
impl azure_core::http::HttpClient for BinaryHttp {
    async fn execute_request(
        &self,
        request: &azure_core::http::Request,
    ) -> azure_core::Result<azure_core::http::AsyncRawResponse> {
        let mut observed = request.clone();
        if let azure_core::http::Body::Bytes(bytes) = request.body()
            && std::str::from_utf8(bytes).is_err()
        {
            self.bytes.lock().unwrap().push(bytes.to_vec());
            // Record the actual SDK bytes before the existing text-only script
            // observer. No HTTP, upload or production request is replaced.
            observed.set_body(b"binary-recorded-by-outer-observer".to_vec());
        }
        azure_core::http::HttpClient::execute_request(self.base.as_ref(), &observed).await
    }
}

#[tokio::test]
async fn remote_binary_upload_preserves_exact_bytes_at_real_azure_http_boundary() {
    let mut host = host();
    remote_metadata(&host, definition("bytes.bin", "/w0KAA=="));
    host.reply(201, Value::Null);
    host.reply(202, Value::Null);
    host.reply(200, Value::Null);
    let http = Arc::new(BinaryHttp {
        base: host.http.clone(),
        bytes: Mutex::default(),
    });
    let mut files = fabric_cli::families::transfer_files::TokioTransferFiles::new(
        host.root.clone(),
        None,
        Default::default(),
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &[
            "fab-rs",
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files",
        ],
        http.clone(),
    )
    .await;
    success(&output);
    assert_eq!(*http.bytes.lock().unwrap(), vec![vec![255, 13, 10, 0]]);
    assert!(
        host.requests()[8]["url"]
            .as_str()
            .unwrap()
            .ends_with("action=flush&position=4")
    );
    assert!(
        matches!(&output.evidence,FamilyEvidence::Transfer(Ok(result))
        if result.completed_paths == ["Book.DataPipeline/bytes.bin"] && result.exported_ids == [NOTEBOOK])
    );
}

fn paged_metadata(host: &Harness) {
    host.reply(
        200,
        json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
    );
    host.reply(200, json!({"value":[],"continuationToken":"next+/="}));
    host.reply(
        200,
        json!({"value":[{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]}),
    );
}

#[tokio::test]
async fn selected_context_binds_export_and_bulk_initial_pages_poll_and_result() {
    for selected in [
        "api.fabric.microsoft.com",
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        for bulk in [false, true] {
            let mut host = host();
            paged_metadata(&host);
            if !bulk {
                host.reply(200, json!({"id":PIPELINE}));
            }
            let root = format!("https://{selected}");
            let poll = format!("{root}/v1/operations/{OPERATION}");
            let result = format!("{poll}/result");
            host.headers(
                202,
                Value::Null,
                &[
                    ("Location", &poll),
                    ("x-ms-operation-id", OPERATION),
                    ("Retry-After", "0"),
                ],
            );
            host.headers(200, json!({"status":"Succeeded"}), &[("Location", &result)]);
            host.reply(
                200,
                if bulk {
                    json!({
                        "definitionParts":[{"path":"/Book.DataPipeline/part.txt","payload":"eA=="}],
                        "itemDefinitionsIndex":[{"id":PIPELINE,"rootPath":"/Book.DataPipeline"}],
                    })
                } else {
                    definition("part.txt", "eA==")
                },
            );
            let mut files = fabric_cli::families::transfer_files::TokioTransferFiles::new(
                host.root.clone(),
                None,
                Default::default(),
                tokio::runtime::Handle::current(),
            )
            .await
            .unwrap();
            let http = host.http.clone();
            let args: &[&str] = if bulk {
                &[
                    "fab-rs",
                    "bulk-export",
                    "Demo.Workspace",
                    "--recursive",
                    "-f",
                    "-o",
                    "output",
                ]
            } else {
                &[
                    "fab-rs",
                    "export",
                    "Demo.Workspace/Book.DataPipeline",
                    "-f",
                    "-o",
                    "output",
                ]
            };
            let output = injected_export_at_endpoint(
                &mut host,
                &mut files,
                pending(),
                args,
                http,
                fabric_sdk::EndpointBinding::with_host(fabric_sdk::ServiceOrigin::Fabric, selected)
                    .unwrap(),
            )
            .await;
            success(&output);
            let requests = host.requests();
            assert_eq!(requests.len(), if bulk { 6 } else { 7 });
            assert!(requests.iter().all(|request| {
                request["url"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("{root}/"))
            }));
            assert!(
                requests[2]["url"]
                    .as_str()
                    .unwrap()
                    .ends_with("continuationToken=next%2B%2F%3D")
            );
            assert_eq!(requests[requests.len() - 2]["url"], poll);
            assert_eq!(requests.last().unwrap()["url"], result);
            assert_eq!(
                fs::read(
                    host.root
                        .join("output")
                        .join("Book.DataPipeline")
                        .join("part.txt")
                )
                .unwrap(),
                b"x"
            );
        }
    }
}

#[tokio::test]
async fn selected_export_context_rejects_foreign_poll_or_result_without_fallback_or_replay() {
    for bulk in [false, true] {
        for foreign_result in [false, true] {
            let host = host();
            paged_metadata(&host);
            if !bulk {
                host.reply(200, json!({"id":PIPELINE}));
            }
            selected_export_foreign_case(host, bulk, foreign_result).await;
        }
    }
}

#[derive(Default)]
struct PreconditionUi {
    events: Mutex<Vec<Value>>,
    confirmations: Mutex<std::collections::VecDeque<bool>>,
}

impl TransferUi for PreconditionUi {
    fn emit(&self, event: TransferEvent) -> Result<(), CommandError> {
        let (kind, message) = match event {
            TransferEvent::Warning(message) => ("warning", message),
            TransferEvent::Grey(message) => ("grey", message),
        };
        self.events
            .lock()
            .unwrap()
            .push(json!({"kind":kind,"message":message}));
        Ok(())
    }

    fn confirm<'a>(&'a self, question: &'a str) -> TransferFuture<'a, Result<bool, CommandError>> {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap()
                .push(json!({"kind":"confirm","message":question}));
            Ok(self
                .confirmations
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected source confirmation"))
        })
    }

    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>> {
        Box::pin(async { panic!("bulk preconditions do not use export selection UI") })
    }
}

#[tokio::test]
async fn actual_python_bulk_precondition_branches_match_real_root_arguments_fs_and_ui() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../fabric-deploy/tests/definition_export_precondition_source.json"
    ))
    .unwrap();
    for case in receipt["cases"].as_array().unwrap() {
        let mut host = host();
        if case["nonempty"].as_bool().unwrap() {
            fs::write(host.root.join("output").join("existing.txt"), b"keep").unwrap();
        }
        let recursive = case["recursive"].as_bool().unwrap();
        if recursive {
            host.reply(
                200,
                json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
            );
        }
        if case["kind"] == "lakehouse" {
            host.metadata_with_id(
                WS,
                json!([{"id":LAKE,"displayName":"Lake","type":"Lakehouse"}]),
            );
            host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
        } else if case["result"] == true {
            host.reply(
                200,
                json!({"value":[{"id":PIPELINE,"displayName":"Book","type":"DataPipeline"}]}),
            );
            host.reply(
                200,
                json!({
                    "definitionParts":[{"path":"/Book.DataPipeline/part.txt","payload":"eA=="}],
                    "itemDefinitionsIndex":[{"id":PIPELINE,"rootPath":"/Book.DataPipeline"}],
                }),
            );
        }
        let ui = PreconditionUi::default();
        if !case["force"].as_bool().unwrap() {
            ui.confirmations
                .lock()
                .unwrap()
                .push_back(case["confirmation"].as_bool().unwrap());
        }
        let mut args = vec![
            "bulk-export",
            "Demo.Workspace",
            "-o",
            case["output"].as_str().unwrap(),
        ];
        if recursive {
            args.push("--recursive");
        }
        if case["force"].as_bool().unwrap() {
            args.push("-f");
        }
        let output = host.execute_with_capabilities(&args, pending(), &ui).await;
        if !case["error"].is_null() {
            failed(&output);
            let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
                unreachable!()
            };
            let envelope = error.envelope(
                fabric_cli_output::Timestamp::parse("2026-10-28T12:00:00Z").unwrap(),
                None,
            );
            assert_eq!(
                envelope.result().message(),
                case["error"]["message"].as_str(),
                "{}",
                case["name"]
            );
            assert_eq!(
                envelope.result().error_code(),
                case["error"]["code"].as_str(),
                "{}",
                case["name"]
            );
            assert!(
                host.requests()
                    .iter()
                    .all(|request| request["method"] != "POST")
            );
        } else {
            success(&output);
            assert!(
                matches!(&output.evidence,FamilyEvidence::Transfer(Ok(result))
                            if result.declined != case["result"].as_bool().unwrap())
            );
            if case["result"] == true {
                assert_eq!(
                    fs::read(
                        host.root
                            .join("output")
                            .join("Book.DataPipeline")
                            .join("part.txt")
                    )
                    .unwrap(),
                    b"x"
                );
            } else {
                assert_eq!(host.requests().len(), 1, "decline must precede discovery");
            }
        }
        let preconditions = ui
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] != "grey")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            Value::Array(preconditions),
            case["events"],
            "{}",
            case["name"]
        );
        assert!(ui.confirmations.lock().unwrap().is_empty());
        if case["nonempty"].as_bool().unwrap() {
            assert_eq!(
                fs::read(host.root.join("output").join("existing.txt")).unwrap(),
                b"keep"
            );
        }
    }
}

#[tokio::test]
async fn item_in_fabric_folder_exports_ordered_json_and_binary_to_nested_lakehouse_folder() {
    let mut host = host();
    host.reply(
        200,
        json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
    );
    host.reply(
        200,
        json!({"value":[{"id":"parent","displayName":"Parent"}]}),
    );
    host.reply(200,json!({"value":[{"id":PIPELINE,"displayName":"Book","type":"DataPipeline","folderId":"parent"}]}));
    host.reply(200, json!({"id":PIPELINE}));
    host.reply(
        200,
        json!({"definition":{"parts":[
            {"path":"definition/object.json","payload":"eyJ6Ijoiw6kiLCJhIjpbMSx0cnVlXX0="},
            {"path":"assets/image.bin","payload":"/w0KAA=="},
        ]}}),
    );
    host.metadata_with_id(
        WS,
        json!([{"id":LAKE,"displayName":"Lake","type":"Lakehouse"}]),
    );
    for _ in 0..2 {
        host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
    }
    for _ in 0..2 {
        host.reply(201, Value::Null);
        host.reply(202, Value::Null);
        host.reply(200, Value::Null);
    }
    let http = Arc::new(BinaryHttp {
        base: host.http.clone(),
        bytes: Mutex::default(),
    });
    let mut files = fabric_cli::families::transfer_files::TokioTransferFiles::new(
        host.root.clone(),
        None,
        Default::default(),
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    let output = injected_export_with_http(
        &mut host,
        &mut files,
        pending(),
        &[
            "fab-rs",
            "export",
            "Demo.Workspace/Parent.Folder/Book.DataPipeline",
            "-f",
            "-o",
            "Demo.Workspace/Lake.Lakehouse/Files/Archive/Run",
        ],
        http.clone(),
    )
    .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 15);
    assert!(
        requests[4]["url"]
            .as_str()
            .unwrap()
            .ends_with("/getDefinition")
    );
    assert_eq!(requests[4]["raw_body"], "");
    assert_eq!(requests[7]["method"], "HEAD");
    assert_eq!(requests[8]["method"], "HEAD");
    assert!(requests[9]["url"].as_str().unwrap().contains(&format!(
        "/{WS}/{LAKE}/Files/Archive/Run/Book.DataPipeline/definition/object.json/?resource=file"
    )));
    assert_eq!(
        requests[10]["raw_body"],
        "{\"z\": \"\\u00e9\", \"a\": [1, true]}"
    );
    assert!(
        requests[12]["url"]
            .as_str()
            .unwrap()
            .contains("/Files/Archive/Run/Book.DataPipeline/assets/image.bin/?resource=file")
    );
    assert_eq!(*http.bytes.lock().unwrap(), vec![vec![255, 13, 10, 0]]);
    assert!(
        matches!(&output.evidence,FamilyEvidence::Transfer(Ok(result))
        if result.exported_ids == [PIPELINE] && result.completed_paths == [
            "Book.DataPipeline/definition/object.json","Book.DataPipeline/assets/image.bin",
        ])
    );
    assert!(!host.root.join("Demo.Workspace").exists());
    assert!(
        fs::read_dir(host.root.join("output"))
            .unwrap()
            .next()
            .is_none()
    );
}
async fn selected_export_foreign_case(mut host: Harness, bulk: bool, foreign_result: bool) {
    let root = "https://msitapi.fabric.microsoft.com";
    let poll = format!("{root}/v1/operations/{OPERATION}");
    let foreign = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    host.headers(
        202,
        Value::Null,
        &[
            ("Location", if foreign_result { &poll } else { &foreign }),
            ("x-ms-operation-id", OPERATION),
            ("Retry-After", "0"),
        ],
    );
    if foreign_result {
        host.headers(
            200,
            json!({"status":"Succeeded"}),
            &[("Location", &format!("{foreign}/result"))],
        );
    }
    let mut files = fabric_cli::families::transfer_files::TokioTransferFiles::new(
        host.root.clone(),
        None,
        Default::default(),
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    let http = host.http.clone();
    let args: &[&str] = if bulk {
        &[
            "fab-rs",
            "bulk-export",
            "Demo.Workspace",
            "--recursive",
            "-f",
            "-o",
            "output",
        ]
    } else {
        &[
            "fab-rs",
            "export",
            "Demo.Workspace/Book.DataPipeline",
            "-f",
            "-o",
            "output",
        ]
    };
    let output = injected_export_at_endpoint(
        &mut host,
        &mut files,
        pending(),
        args,
        http,
        fabric_sdk::EndpointBinding::with_host(
            fabric_sdk::ServiceOrigin::Fabric,
            "msitapi.fabric.microsoft.com",
        )
        .unwrap(),
    )
    .await;
    failed(&output);
    let requests = host.requests();
    assert_eq!(
        requests.len(),
        (if bulk { 4 } else { 5 }) + usize::from(foreign_result)
    );
    assert!(
        requests
            .iter()
            .all(|request| request["url"].as_str().unwrap().starts_with(root))
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        1
    );
    assert!(
        fs::read_dir(host.root.join("output"))
            .unwrap()
            .next()
            .is_none()
    );
    let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
        unreachable!()
    };
    let TransferFailure::Definition(error) = &error.failure else {
        unreachable!()
    };
    let fabric_deploy::definitions::DefinitionErrorKind::Operation(error) = &error.kind else {
        unreachable!()
    };
    assert_eq!(
        error.operation_id.as_deref(),
        foreign_result.then_some(OPERATION)
    );
    assert!(error.mutation_may_have_occurred);
}

#[tokio::test]
async fn central_transfer_support_gate_rejects_source_scopes_and_types_before_preparation() {
    for (leaf, target, folder, unsupported_type) in [
        ("import", "Demo.Workspace", false, false),
        ("import", "Demo.Workspace/Parent.Folder", true, false),
        ("export", "Demo.Workspace/Parent.Folder", true, false),
        ("import", "Demo.Workspace/Book.GraphQuerySet", false, true),
    ] {
        let mut host = host();
        host.reply(
            200,
            json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
        );
        if folder {
            host.reply(
                200,
                json!({"value":[{"id":NOTEBOOK,"displayName":"Parent"}]}),
            );
        }
        if unsupported_type {
            host.reply(
                200,
                json!({"value":[{"id":PIPELINE,"displayName":"Book","type":"GraphQuerySet"}]}),
            );
        }
        let args = if leaf == "import" {
            vec![leaf, target, "-i", "must-not-be-read", "-f"]
        } else {
            vec![leaf, target, "-o", "output", "-f"]
        };
        let output = host.execute(&args).await;
        failed(&output);
        let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
            unreachable!()
        };
        let envelope = error.envelope(
            fabric_cli_output::Timestamp::parse("2026-10-28T12:00:00Z").unwrap(),
            None,
        );
        assert_eq!(envelope.result().error_code(), Some("UnsupportedCommand"));
        assert_eq!(
            envelope.result().message(),
            Some(format!("not supported for command '{leaf}'").as_str())
        );
        assert!(
            host.requests()
                .iter()
                .all(|request| request["method"] == "GET")
        );
        assert!(
            fs::read_dir(host.root.join("output"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[tokio::test]
async fn central_import_support_gate_allows_known_missing_item_and_retains_folder_context() {
    let mut host = host();
    fs::create_dir(host.root.join("input")).unwrap();
    fs::write(host.root.join("input").join("part.txt"), b"raw").unwrap();
    host.reply(
        200,
        json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
    );
    host.reply(
        200,
        json!({"value":[{"id":NOTEBOOK,"displayName":"Parent"}]}),
    );
    host.reply(200, json!({"value":[]}));
    let created = "88888888-8888-8888-8888-888888888888";
    host.reply(201, json!({"id":created}));
    let output = host
        .execute(&[
            "import",
            "Demo.Workspace/Parent.Folder/New.DataPipeline",
            "-i",
            "input",
            "-f",
        ])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[3]["method"], "POST");
    assert_eq!(requests[3]["body"]["folderId"], NOTEBOOK);
    assert_eq!(requests[3]["body"]["displayName"], "New");
    assert_eq!(requests[3]["body"]["type"], "DataPipeline");
    assert_eq!(
        requests[3]["body"]["definition"]["parts"],
        json!([
            {"path":"part.txt","payload":"cmF3","payloadType":"InlineBase64"},
        ])
    );
    assert!(
        matches!(&output.evidence,FamilyEvidence::Transfer(Ok(result))
        if result.effects.len() == 1 && result.effects[0].item_id == created
        && result.effects[0].target.id().is_none()
        && result.effects[0].target.parent().and_then(|parent| parent.id()) == Some(NOTEBOOK))
    );
}
