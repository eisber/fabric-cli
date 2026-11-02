// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent hand-authored expectations shared with the unchanged-source oracle.

use fabric_cli_output::{Indent, Timestamp};
use fabric_cli_tables::{TableCommand, TableOperation, command, load_options, optimize, vacuum};
use fabric_sdk_tables::{SourceKind, TablePath};
use serde_json::Value;

#[test]
fn source_options_and_configuration_match_independent_inventory() {
    let inventory: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/cases.json"
    ))
    .unwrap();
    let branches: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/branches.json"
    ))
    .unwrap();
    let decimal: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/retention-unicode.json"
    ))
    .unwrap();
    for case in inventory["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(branches["cases"].as_array().unwrap().iter())
        .chain(decimal["cases"].as_array().unwrap().iter())
    {
        let table = TablePath::new(
            "ws",
            "item",
            case["local_path"].as_str().unwrap_or("Tables/sales"),
        )
        .unwrap();
        let result = match case["operation"].as_str().unwrap() {
            "load" => {
                if case.get("load_status").is_some() {
                    continue;
                }
                if case["source_kind"] == "unknown" {
                    continue;
                }
                load_options(
                    case["mode"].as_str().unwrap_or("overwrite"),
                    case["format"].as_str(),
                    case["extension"].as_str(),
                )
                .map(|options| {
                    options.payload(
                        &TablePath::new("ws", "item", "Files/input.csv").unwrap(),
                        if case["source_kind"] == "directory" {
                            SourceKind::Folder
                        } else {
                            SourceKind::File
                        },
                    )
                })
            }
            "vacuum" => {
                vacuum(case["hours"].as_str().unwrap()).map(|config| config.execution_data(&table))
            }
            "optimize" => Ok(optimize(
                case["vorder"].as_bool().unwrap_or(false),
                case["zorder"].as_str(),
            )
            .execution_data(&table)),
            _ => continue,
        };
        if let Some(expected) = case.get("expected") {
            assert_eq!(&result.unwrap(), expected, "{}", case["id"]);
        } else {
            let plan = result
                .unwrap_err()
                .envelope(
                    Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                    Some("table".into()),
                )
                .render(Indent::Four)
                .unwrap();
            let value: Value = serde_json::from_slice(&plan.payload).unwrap();
            assert_eq!(
                value["result"]["error_code"], case["error_code"],
                "{}",
                case["id"]
            );
            assert_eq!(plan.exit_code, 1);
        }
    }
}

#[test]
fn registration_preserves_source_cardinality_choices_and_maintenance_flags() {
    let parsed = command()
        .try_get_matches_from([
            "table",
            "load",
            "Tables/my",
            "table",
            "--file",
            "Files/my",
            "file",
            "--mode",
            "append",
        ])
        .unwrap();
    let parsed = TableCommand::from_matches(&parsed).unwrap();
    assert_eq!(parsed.path, ["Tables/my", "table"]);
    assert!(
        matches!(parsed.operation, TableOperation::Load { mode, file, .. } if mode == "append" && file == ["Files/my","file"])
    );
    assert!(
        command()
            .try_get_matches_from(["table", "load", "Tables/a"])
            .is_err()
    );
    assert!(
        command()
            .try_get_matches_from([
                "table", "load", "Tables/a", "--file", "Files/a", "--mode", "Append"
            ])
            .is_err()
    );
    assert!(
        command()
            .try_get_matches_from(["table", "vacuum", "Tables/a", "--dry-run"])
            .is_err()
    );
    assert!(
        command()
            .try_get_matches_from(["table", "schema", "Tables/a", "--query", "fields"])
            .is_err()
    );
    let parsed = command()
        .try_get_matches_from(["table", "vacuum", "Tables/a", "--retain_n_hours", "-1"])
        .unwrap();
    assert!(
        matches!(TableCommand::from_matches(&parsed).unwrap().operation, TableOperation::Vacuum { retain_n_hours } if retain_n_hours == "-1")
    );
    assert!(
        TableCommand::from_matches(&command().try_get_matches_from(["table"]).unwrap()).is_none()
    );
    let parsed = command()
        .try_get_matches_from([
            "table",
            "load",
            "Tables/a",
            "--file",
            "Files/a",
            "--mode",
            "append",
            "--mode",
            "overwrite",
            "--form",
            "format=csv",
        ])
        .unwrap();
    assert!(
        matches!(TableCommand::from_matches(&parsed).unwrap().operation, TableOperation::Load {mode,format,..} if mode=="overwrite" && format.as_deref()==Some("format=csv"))
    );
}

#[test]
fn retention_is_not_narrowed_to_a_machine_integer() {
    let table = TablePath::new("ws", "item", "Tables/sales").unwrap();
    let hours = format!("24{}", "0".repeat(60));
    assert_eq!(
        vacuum(&hours).unwrap().execution_data(&table)["vacuumSettings"]["retentionPeriod"],
        format!("1{}:00:00:00", "0".repeat(60))
    );
    assert_eq!(
        vacuum(&format!("-{hours}")).unwrap().execution_data(&table)["vacuumSettings"]["retentionPeriod"],
        format!("-1{}:00:00:00", "0".repeat(60))
    );
    assert!(vacuum("--1").is_err());
    assert!(vacuum("1__2").is_err());
}

#[test]
fn retention_digit_limit_is_exact_and_can_be_explicitly_disabled() {
    let table = TablePath::new("ws", "item", "Tables/sales").unwrap();
    assert_eq!(
        vacuum(&"0".repeat(4300)).unwrap().execution_data(&table)["vacuumSettings"]["retentionPeriod"],
        "0:00:00:00"
    );
    assert!(vacuum(&"0".repeat(4301)).is_err());
    assert_eq!(
        fabric_cli_tables::vacuum_with_digit_limit(&"0".repeat(4301), None)
            .unwrap()
            .execution_data(&table)["vacuumSettings"]["retentionPeriod"],
        "0:00:00:00"
    );
}
