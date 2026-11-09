// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native Warehouse definition request contracts, without a runtime or transport.

#![deny(unnameable_test_items)]

use fabric_sdk_sql_database::warehouse::{
    WarehouseDefinitionFormat as Format, WarehouseRead, WarehouseRequests, WarehouseTarget,
};
use fabric_sdk_sql_definition::{DefinitionErrorKind, DefinitionLimits};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use std::num::NonZeroUsize;

const WS: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn limits() -> DefinitionLimits {
    let bound = NonZeroUsize::new(1024).unwrap();
    DefinitionLimits {
        max_wire_bytes: bound,
        max_decoded_bytes: bound,
        max_part_bytes: bound,
        max_parts: bound,
        max_path_bytes: bound,
        max_depth: bound,
    }
}

#[test]
fn native_post_query_body_owner_scopes_and_nonpolling_policy_are_exact() {
    let owner =
        WarehouseTarget::new(WS.parse().unwrap(), ITEM.parse().unwrap(), "Warehouse").unwrap();
    for (format, query) in [
        (None, ""),
        (Some(Format::Sqlproj), "?format=sqlproj"),
        (Some(Format::Dacpac), "?format=dacpac"),
    ] {
        let request = WarehouseRequests::get_definition(&owner, format, limits()).unwrap();
        assert_eq!(request.owner(), owner);
        assert_eq!(request.format(), format.unwrap_or_default());
        assert_eq!(
            request.operation().path(),
            format!("/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition{query}")
        );
        assert_eq!(request.operation().method(), ResourceMethod::Post);
        assert_eq!(
            request.operation().completion(),
            CompletionPolicy::ReturnAccepted
        );
        assert!(request.operation().body().is_none());
        assert_eq!(
            request.delegated_scopes(),
            ["Warehouse.ReadWrite.All", "Item.ReadWrite.All"]
        );
        assert_eq!(request.supported_identities().len(), 3);
    }
    assert_eq!(WarehouseRead::ALL.len(), 5);
    assert!(
        WarehouseRead::ALL
            .iter()
            .all(|read| read.method() == ResourceMethod::Get)
    );
    for invalid in [
        "warehouse",
        "SQLDatabase",
        "WarehouseSnapshot",
        "SQLEndpoint",
        "Lakehouse",
    ] {
        assert!(WarehouseTarget::new(WS.parse().unwrap(), ITEM.parse().unwrap(), invalid).is_err());
    }
}

#[test]
fn completed_result_retains_selected_owner_without_fabricating_service_echo() {
    let owner =
        WarehouseTarget::new(WS.parse().unwrap(), ITEM.parse().unwrap(), "Warehouse").unwrap();
    let request = WarehouseRequests::get_definition(&owner, None, limits()).unwrap();
    let body = br#"{"definition":{"parts":[{"path":"sqldw.sqlproj","payload":"eA==","payloadType":"InlineBase64"}]}}"#;
    let result = request.decode_completed_result(body).unwrap();
    assert_eq!(result.owner, owner);
    assert_eq!(result.data.response_bytes(), body);
    assert_eq!(result.data.format(), Format::Sqlproj);
    for incomplete in [b"{}".as_slice(), b"{\"status\":\"Running\"}"] {
        assert_eq!(
            request
                .decode_completed_result(incomplete)
                .unwrap_err()
                .kind,
            DefinitionErrorKind::MissingDefinition
        );
    }
}
