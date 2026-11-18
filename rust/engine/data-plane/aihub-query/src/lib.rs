// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, caller-runtime AIHub MCP queries with explicit DataAgent typed JSON.
//!
//! No endpoint, credential, audience, tenant, account, stage or datasource is
//! discovered. Endpoint/auth compatibility must be qualified by the caller.
//! Dropping a future stops local observation, not remote execution or billing.

mod catalog;
mod client;
mod contract;
mod decode;
mod json;
mod media;
mod protocol;
mod workbook;

pub use catalog::{CatalogView, NativeCatalogContent, NativeCatalogResponse};
pub use client::{AiHubQueryClient, AuthBinding};
pub use contract::{
    Completeness, DataAgentTarget, Limits, QueryError, QueryErrorKind, QueryMode, QueryOutcome,
    NativeWorkbookRequest, QueryRequest, QueryStage, TypedJson,
};
pub use decode::{Column, EmbeddedResource, NativeValue, QueryResource, TypedQueryResult};
pub use fabric_sdk::runtime::NativeDeadline;
pub use fabric_sdk_http::ExecutionTimer;
pub use workbook::{NativeWorkbook, NativeWorkbookResponse, WORKBOOK_MIME};

/// Existing DataAgent routing wire variant; this is not an eligibility grant.
pub const DATA_AGENT_VARIANT: &str = "Fabric.Routing.Documents.DataAgent";
/// Existing typed-result wire variant; it does not select a stage or datasource.
pub const TYPED_RESULTS_VARIANT: &str = "Fabric.Routing.Documents.DataAgent.TypedQueryResults";
/// Existing one-way SQL/GQL read-policy request; not live enforcement proof.
pub const READ_ONLY_VARIANT: &str = "Fabric.Routing.Documents.SqlReadOnlyEnforce";
/// Existing explicit draft-stage wire variant.
pub const SANDBOX_VARIANT: &str = "Fabric.Routing.Documents.DataAgent.Sandbox";
/// Existing curated-template wire variant, used only for template requests.
pub const TEMPLATE_VARIANT: &str = "Fabric.Routing.Documents.QueryTemplates";
