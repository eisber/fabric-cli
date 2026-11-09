// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse management reads over the caller's shared Fabric operation policy.
//!
//! Connection metadata is inert: no SQL session, catalog fallback, credential
//! selection, runtime, mutation, monitor traversal or independent pager exists.

mod ids;
mod models;
mod requests;

pub use ids::{RestorePointId, WarehouseId, WarehouseTarget};
pub use models::{
    ConnectionMetadata, Owned, RestorePoint, RestorePointPage, Warehouse, WarehousePage,
    WarehouseProperties,
};
pub use requests::{
    ConnectionOptions, ListWarehouses, PrivateLinkType, WarehouseError, WarehousePermission,
    WarehouseRead, WarehouseRequest, WarehouseRequests,
};
