// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse management reads over the caller's shared Fabric operation policy.
//!
//! Connection metadata is inert: no SQL session, catalog fallback, credential
//! selection, runtime, mutation, monitor traversal or independent pager exists.
//! Native definition retrieval is a separate POST descriptor requiring read/write.

mod definition;
mod ids;
mod models;
mod requests;

pub use definition::{WarehouseDefinitionIdentity, WarehouseDefinitionRequest};
pub use fabric_sdk_sql_definition::warehouse::{
    NativeMediaType, WarehouseDefinition, WarehouseDefinitionFormat,
};
pub use ids::{RestorePointId, WarehouseId, WarehouseTarget};
pub use models::{
    ConnectionMetadata, Owned, RestorePoint, RestorePointPage, Warehouse, WarehousePage,
    WarehouseProperties,
};
pub use requests::{
    ConnectionOptions, ListWarehouses, PrivateLinkType, WarehouseError, WarehousePermission,
    WarehouseRead, WarehouseRequest, WarehouseRequests,
};
