// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_sdk_tables::lakehouses::tables::TableOperation;

#[test]
fn facade_discovery_is_parent_owned_and_uses_the_same_contract() {
    let lakehouse = fabric_sdk_tables::lakehouses::tables::describe(TableOperation::Load);
    let kusto = fabric_sdk_tables::kql_databases::tables::describe(TableOperation::Load);
    let warehouse = fabric_sdk_tables::warehouses::tables::describe(TableOperation::Load);
    assert_eq!(lakehouse["parentType"], "Lakehouse");
    assert_eq!(kusto["parentType"], "KQLDatabase");
    assert_eq!(warehouse["parentType"], "Warehouse");
    assert_eq!(lakehouse["transport"], "fabricLoadOperation");
    assert_eq!(kusto["transport"], "kustoManagement");
    assert_eq!(warehouse["transport"], "tds");
    for description in [lakehouse, kusto, warehouse] {
        assert_eq!(description["subresourceType"], "table");
        assert_eq!(description["verb"], "load");
    }
}

#[test]
fn parent_targets_and_load_options_are_reachable_without_a_table_facade() {
    use fabric_sdk_tables::{kql_databases, lakehouses, warehouses};

    let workspace = "11111111-1111-4111-8111-111111111111";
    let item = "22222222-2222-4222-8222-222222222222";
    let table = lakehouses::tables::LakehouseTable::new(workspace, item, None, "orders").unwrap();
    assert_eq!(table.workspace(), workspace);
    assert_eq!(table.lakehouse(), item);
    let _load = lakehouses::tables::LakehouseLoadOptions::new(
        lakehouses::tables::LoadMode::Append,
        lakehouses::tables::LakehouseLoadFormat::Parquet,
    );
    let _maintenance = lakehouses::tables::LakehouseMaintenanceOptions::vacuum();
    let _wait = lakehouses::tables::jobs::WaitOptions::default();

    let binding = kql_databases::tables::KustoBinding::new(
        workspace,
        item,
        "database",
        "https://cluster.westeurope.kusto.fabric.microsoft.com",
    )
    .unwrap();
    assert_eq!(binding.database(), "database");
    let _table = kql_databases::tables::KustoTable::new("orders").unwrap();

    let connection = warehouses::tables::WarehouseConnection::new(
        "example.datawarehouse.fabric.microsoft.com",
        "warehouse",
    )
    .unwrap();
    assert_eq!(connection.initial_catalog(), "warehouse");
    let _table = warehouses::tables::WarehouseTable::new("sales", "orders").unwrap();
    let _load = warehouses::tables::CopyIntoOptions::new(
        warehouses::tables::CopyFileType::Parquet,
        warehouses::tables::CopySourceAuthorization::CallerIdentity,
    );
}
