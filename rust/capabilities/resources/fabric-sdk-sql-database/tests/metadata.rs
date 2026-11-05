// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public SQLDatabase metadata request and response contracts.

#![deny(unnameable_test_items)]

use fabric_sdk_sql_database::{
    ContinuationToken, DelegatedScopeAlternatives, FolderId, FolderScope, Guid, ListSqlDatabases,
    ManagementEffect, MetadataPermission, Observed, PageCoverage, SqlDatabaseId,
    SqlDatabaseRequests, SqlDatabaseTarget, SqlInputError, SqlManagementError, SqlRequest,
    SqlResponseErrorKind, WorkspaceId,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOutcome};
use serde_json::{Value, json};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const DATABASE: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OTHER: &str = "00000000-0000-0000-0000-000000000000";

fn target() -> SqlDatabaseTarget {
    SqlDatabaseTarget::new(
        WORKSPACE.parse().unwrap(),
        DATABASE.parse().unwrap(),
        "SQLDatabase",
    )
    .unwrap()
}

fn row() -> Value {
    json!({
        "id": DATABASE,
        "workspaceId": WORKSPACE,
        "displayName": "sensitive-display-name",
        "type": "SQLDatabase",
    })
}

fn completed(data: Value) -> ResourceOutcome {
    ResourceOutcome::Completed {
        status: 200,
        data: Some(data),
    }
}

fn response_kind<T>(request: &SqlRequest<T>, data: Value) -> SqlResponseErrorKind {
    match request.decode(completed(data)) {
        Err(SqlManagementError::Response(error)) => error.kind,
        _ => panic!("expected typed response error"),
    }
}

#[test]
fn native_guids_are_strict_and_semantic_types_remain_distinct() {
    let uppercase = DATABASE.to_uppercase();
    assert_eq!(uppercase.parse::<Guid>().unwrap().to_string(), DATABASE);
    assert_eq!(OTHER.parse::<Guid>().unwrap().to_string(), OTHER);
    for invalid in [
        "".to_owned(),
        format!(" {DATABASE}"),
        format!("{{{DATABASE}}}"),
        DATABASE.replace('-', ""),
        DATABASE.replace('-', "_"),
        DATABASE.replacen('a', "g", 1),
        DATABASE.replacen('a', "%61", 1),
        format!("{DATABASE}/.."),
        format!("{DATABASE}\n"),
        "é".repeat(18),
    ] {
        assert!(invalid.parse::<Guid>().is_err());
        assert!(invalid.parse::<WorkspaceId>().is_err());
        assert!(invalid.parse::<SqlDatabaseId>().is_err());
        assert!(invalid.parse::<FolderId>().is_err());
    }
    let workspace: WorkspaceId = WORKSPACE.parse().unwrap();
    let database: SqlDatabaseId = DATABASE.parse().unwrap();
    for owner in [
        "Warehouse",
        "SQLEndpoint",
        "sqlDatabase",
        "",
        "SQLDatabase ",
    ] {
        assert_eq!(
            SqlDatabaseTarget::new(workspace, database, owner),
            Err(SqlInputError::WrongItemType)
        );
    }
}

#[test]
fn exact_get_and_list_routes_have_no_bodies_or_hidden_waits() {
    let get = SqlDatabaseRequests::get(&target()).unwrap();
    assert_eq!(
        get.operation().path(),
        format!("/v1/workspaces/{WORKSPACE}/sqlDatabases/{DATABASE}")
    );
    assert_eq!(get.operation().method(), ResourceMethod::Get);
    assert!(get.operation().body().is_none());
    assert_eq!(
        get.operation().completion(),
        CompletionPolicy::ReturnAccepted
    );
    assert_eq!(get.semantics().effect, ManagementEffect::ReadMetadata);
    assert_eq!(get.semantics().permission, MetadataPermission::ItemRead);
    assert_eq!(
        get.semantics().scopes,
        DelegatedScopeAlternatives::SqlDatabaseOrItemReadOrReadWrite
    );

    for recursive in [false, true] {
        let list = SqlDatabaseRequests::list_page(
            &WORKSPACE.parse().unwrap(),
            &ListSqlDatabases {
                recursive,
                root_folder: FolderScope::Workspace,
                continuation_token: None,
            },
        )
        .unwrap();
        assert_eq!(
            list.operation().path(),
            format!("/v1/workspaces/{WORKSPACE}/sqlDatabases?recursive={recursive}")
        );
        assert!(list.operation().body().is_none());
        assert_eq!(list.operation().method(), ResourceMethod::Get);
        assert_eq!(
            list.operation().completion(),
            CompletionPolicy::ReturnAccepted
        );
        assert_eq!(
            list.semantics().permission,
            MetadataPermission::WorkspaceViewer
        );
        assert_eq!(
            list.semantics().scopes,
            DelegatedScopeAlternatives::WorkspaceReadOrReadWrite
        );
    }
}

#[test]
fn token_is_encoded_exactly_once_and_folder_is_native() {
    let list = SqlDatabaseRequests::list_page(
        &WORKSPACE.parse().unwrap(),
        &ListSqlDatabases {
            recursive: false,
            root_folder: FolderScope::Folder(OTHER.parse().unwrap()),
            continuation_token: Some(ContinuationToken::new("a+b/%2F &?=日").unwrap()),
        },
    )
    .unwrap();
    assert_eq!(
        list.operation().path(),
        format!(
            "/v1/workspaces/{WORKSPACE}/sqlDatabases?recursive=false&rootFolderId={OTHER}\
             &continuationToken=a%2Bb%2F%252F+%26%3F%3D%E6%97%A5"
        )
    );
    for token in ["", "opaque\n", "opaque\0", "opaque\t", "opaque\u{7f}"] {
        assert_eq!(
            ContinuationToken::new(token),
            Err(SqlInputError::InvalidContinuationToken)
        );
    }
}

#[test]
fn required_identity_and_known_types_are_checked_without_fallback() {
    let request = SqlDatabaseRequests::get(&target()).unwrap();
    for field in ["id", "workspaceId", "displayName", "type"] {
        let mut missing = row();
        missing.as_object_mut().unwrap().remove(field);
        assert_eq!(
            response_kind(&request, missing),
            SqlResponseErrorKind::InvalidShape
        );
        let mut null = row();
        null[field] = Value::Null;
        assert_eq!(
            response_kind(&request, null),
            SqlResponseErrorKind::InvalidShape
        );
    }
    for data in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"definition":{"parts":[]}}),
        {
            let mut data = row();
            data["type"] = json!("Warehouse");
            data
        },
        {
            let mut data = row();
            data["properties"] = json!({"backupRetentionDays": true});
            data
        },
        {
            let mut data = row();
            data["properties"] = json!({"serverFqdn": []});
            data
        },
        {
            let mut data = row();
            data["properties"] = json!("not-properties");
            data
        },
    ] {
        assert_eq!(
            response_kind(&request, data),
            SqlResponseErrorKind::InvalidShape
        );
    }
    for field in ["id", "workspaceId"] {
        let mut data = row();
        data[field] = json!(OTHER);
        data["logicalId"] = json!(DATABASE);
        assert_eq!(
            response_kind(&request, data),
            SqlResponseErrorKind::IdentityMismatch
        );
    }
}

#[test]
fn known_optionals_preserve_missing_null_and_inert_properties() {
    let request = SqlDatabaseRequests::get(&target()).unwrap();
    let missing = request.decode(completed(row())).unwrap();
    assert_eq!(missing.item_type(), "SQLDatabase");
    assert_eq!(missing.description, Observed::Missing);
    assert_eq!(missing.properties, Observed::Missing);
    let mut null = row();
    null["properties"] = Value::Null;
    null["description"] = Value::Null;
    let null = request.decode(completed(null)).unwrap();
    assert_eq!(null.properties, Observed::Null);
    assert_eq!(null.description, Observed::Null);

    let mut full = row();
    full["description"] = json!("");
    full["properties"] = json!({
        "databaseName": "actual-catalog-not-displayName",
        "serverFqdn": "inert-host,invalid-port",
        "connectionString": "sensitive-service-connection-string",
        "earliestRestorePoint": "observed-not-a-validated-restore-input",
        "backupRetentionDays": 7,
        "collation": null,
        "futureProperty": {"arbitrary": true}
    });
    let full = request.decode(completed(full)).unwrap();
    assert_eq!(full.description, Observed::Value(String::new()));
    let Observed::Value(properties) = full.properties else {
        panic!("expected properties");
    };
    assert_eq!(
        properties.database_name,
        Observed::Value("actual-catalog-not-displayName".into())
    );
    assert_eq!(
        properties.server_fqdn,
        Observed::Value("inert-host,invalid-port".into())
    );
    assert_eq!(properties.latest_restore_point, Observed::Missing);
    assert_eq!(properties.collation, Observed::Null);
    let Observed::Value(connection) = properties.connection_string else {
        panic!("expected connection metadata");
    };
    assert_eq!(connection.as_str(), "sensitive-service-connection-string");
    assert!(!format!("{connection:?}").contains("sensitive-service"));
}

#[test]
fn unknown_response_fields_are_projection_only() {
    let request = SqlDatabaseRequests::get(&target()).unwrap();
    let original = request.decode(completed(row())).unwrap();
    let mut future = row();
    future["futureWritableLookingField"] = json!({"server": "never-applied"});
    future["sensitivityLabel"] = json!({"futureShape": [1, 2, 3]});
    assert_eq!(request.decode(completed(future)).unwrap(), original);
}

#[test]
fn page_requires_typed_rows_checks_every_owner_and_retains_incomplete_cursor() {
    let request = SqlDatabaseRequests::list_page(
        &WORKSPACE.parse().unwrap(),
        &ListSqlDatabases {
            recursive: true,
            root_folder: FolderScope::Workspace,
            continuation_token: None,
        },
    )
    .unwrap();
    for data in [json!({}), json!({"value":null}), json!({"value":{}})] {
        assert_eq!(
            response_kind(&request, data),
            SqlResponseErrorKind::InvalidShape
        );
    }
    let mut wrong = row();
    wrong["workspaceId"] = json!(OTHER);
    assert_eq!(
        response_kind(&request, json!({"value":[row(), wrong]})),
        SqlResponseErrorKind::IdentityMismatch
    );
    let mut other = row();
    other["id"] = json!(OTHER);
    let page = request
        .decode(completed(json!({
            "value":[row(), other, row()],
            "continuationToken":"sensitive-cursor",
            "continuationUri":"https://untrusted.invalid/never-follow"
        })))
        .unwrap();
    assert_eq!(page.value.len(), 3);
    assert_eq!(page.value[0], page.value[2]);
    assert_eq!(page.value[1].id.to_string(), OTHER);
    assert_eq!(page.coverage(), PageCoverage::Incomplete);
    assert_eq!(
        page.continuation_token,
        Observed::Value("sensitive-cursor".into())
    );
    assert_eq!(
        page.continuation_uri,
        Observed::Value("https://untrusted.invalid/never-follow".into())
    );
    let empty = request.decode(completed(json!({"value":[]}))).unwrap();
    assert!(empty.value.is_empty());
    assert_eq!(empty.continuation_token, Observed::Missing);
    assert_eq!(empty.coverage(), PageCoverage::TerminalPage);
    let null = request
        .decode(completed(json!({
            "value":[], "continuationToken":null, "continuationUri":""
        })))
        .unwrap();
    assert_eq!(null.continuation_token, Observed::Null);
    assert_eq!(null.continuation_uri, Observed::Value(String::new()));
    assert_eq!(null.coverage(), PageCoverage::TerminalPage);
    assert_eq!(
        response_kind(&request, json!({"value":[], "continuationToken":1})),
        SqlResponseErrorKind::InvalidShape
    );
}

#[test]
fn positional_and_object_discriminator_shapes_are_rejected_at_public_boundaries() {
    let get = SqlDatabaseRequests::get(&target()).unwrap();
    let list = SqlDatabaseRequests::list_page(
        &WORKSPACE.parse().unwrap(),
        &ListSqlDatabases {
            recursive: false,
            root_folder: FolderScope::Workspace,
            continuation_token: None,
        },
    )
    .unwrap();
    for data in [
        json!([[]]),
        json!({"value": {}}),
        json!({"value": null}),
        json!({"value": "not-an-array"}),
    ] {
        assert_eq!(
            response_kind(&list, data.clone()),
            SqlResponseErrorKind::InvalidShape
        );
        assert!(serde_json::from_value::<fabric_sdk_sql_database::SqlDatabasePage>(data).is_err());
    }
    let mut invalid_rows = vec![
        json!([]),
        json!([DATABASE, WORKSPACE, "positional-name", "SQLDatabase"]),
        Value::Null,
    ];
    for discriminator in [
        json!({"SQLDatabase": null}),
        json!(["SQLDatabase"]),
        json!(1),
    ] {
        let mut data = row();
        data["type"] = discriminator;
        invalid_rows.push(data);
    }
    for properties in [
        json!([]),
        json!([null, null, null, null, null, null, null]),
        json!({"connectionString": {"unexpected": "object"}}),
        json!({"databaseName": []}),
        json!({"serverFqdn": {}}),
        json!({"earliestRestorePoint": []}),
        json!({"latestRestorePoint": {}}),
        json!({"backupRetentionDays": []}),
        json!({"collation": {}}),
    ] {
        let mut data = row();
        data["properties"] = properties;
        invalid_rows.push(data);
    }
    for data in invalid_rows {
        assert_eq!(
            response_kind(&get, data.clone()),
            SqlResponseErrorKind::InvalidShape
        );
        assert_eq!(
            response_kind(&list, json!({"value": [row(), data.clone()]})),
            SqlResponseErrorKind::InvalidShape
        );
        assert!(serde_json::from_value::<fabric_sdk_sql_database::SqlDatabase>(data).is_err());
    }
    for properties in [json!([]), Value::Null, json!("properties"), json!(1)] {
        assert!(
            serde_json::from_value::<fabric_sdk_sql_database::SqlDatabaseProperties>(properties)
                .is_err()
        );
    }
    let empty_properties =
        serde_json::from_value::<fabric_sdk_sql_database::SqlDatabaseProperties>(json!({}))
            .unwrap();
    assert_eq!(empty_properties.database_name, Observed::Missing);
}

#[test]
fn only_documented_200_is_a_completed_result_and_debug_is_sanitized() {
    let request = SqlDatabaseRequests::get(&target()).unwrap();
    for status in [201, 202, 204, 301, 403, 429, 500] {
        let error = request
            .decode(ResourceOutcome::Completed {
                status,
                data: Some(row()),
            })
            .unwrap_err();
        let SqlManagementError::Response(error) = error else {
            panic!("expected response failure");
        };
        assert_eq!(error.http_status, status);
        assert_eq!(error.kind, SqlResponseErrorKind::UnexpectedStatus);
    }
    let error = request
        .decode(ResourceOutcome::Accepted {
            operation_id: Some("sensitive-operation-handle".into()),
        })
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("sensitive-operation-handle"));
    let SqlManagementError::Response(error) = error else {
        panic!("expected response failure");
    };
    assert_eq!(error.kind, SqlResponseErrorKind::UnexpectedAcceptance);
    assert_eq!(error.http_status, 202);
    assert_eq!(
        error.operation_id.as_deref(),
        Some("sensitive-operation-handle")
    );
    assert_eq!(
        request
            .decode(ResourceOutcome::Completed {
                status: 200,
                data: None
            })
            .unwrap_err(),
        SqlManagementError::Response(fabric_sdk_sql_database::SqlResponseError {
            kind: SqlResponseErrorKind::MissingBody,
            http_status: 200,
            operation_id: None,
        })
    );
    let mut value = row();
    value["description"] = json!("sensitive-description");
    let database = request.decode(completed(value)).unwrap();
    for diagnostic in [
        format!("{database:?}"),
        format!("{:?}", database.description),
        format!("{request:?}"),
        format!("{:?}", ContinuationToken::new("sensitive-cursor").unwrap()),
        format!("{:?}", target()),
    ] {
        for sensitive in [
            "sensitive-description",
            "sensitive-display-name",
            "sensitive-cursor",
            WORKSPACE,
            DATABASE,
        ] {
            assert!(!diagnostic.contains(sensitive));
        }
    }
}
