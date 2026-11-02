// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric catalog search and hidden read routes, not mutations or CLI support policy.

use azure_core::http::Method;
use fabric_sdk_api::{
    ApiClient, ApiError, Collection, CollectionError, Limits, Paging, Request, Response,
    require_status,
};
use serde_json::{Value, json};
use std::{future::Future, num::NonZeroUsize};

mod metadata;
pub use metadata::{HiddenMetadata, MetadataOmission};

fn search_payload(response: &Response) -> Result<Value, ApiError> {
    let value: Value = serde_json::from_slice(&response.body).map_err(|_| {
        if response.status == 200 {
            ApiError::InvalidJson
        } else {
            ApiError::Service {
                status: response.status,
                code: "InvalidJson".into(),
                message: String::from_utf8_lossy(&response.body).into_owned(),
            }
        }
    })?;
    if response.status == 200 {
        return Ok(value);
    }
    if (200..300).contains(&response.status) {
        return Err(ApiError::UnsupportedStatus(response.status));
    }
    let code = value
        .get("errorCode")
        .and_then(Value::as_str)
        .unwrap_or("UnexpectedError");
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(&response.body).into_owned());
    Err(ApiError::Service {
        status: response.status,
        code: code.into(),
        message,
    })
}

/// Typed catalog search body; CLI type validation is deliberately outside SDK.
#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Text searched by the service across source catalog fields.
    pub search: String,
    /// Explicit finite source page size (CLI 1000, interactive 30).
    pub page_size: NonZeroUsize,
    /// Optional OData filter, not locally reinterpreted.
    pub filter: Option<String>,
}

impl SearchOptions {
    /// Builds the first request or the source continuation-only body.
    pub fn request(&self, continuation: Option<&str>) -> Result<Request, ApiError> {
        let payload = if let Some(token) = continuation {
            json!({"continuationToken":token,"pageSize":self.page_size.get()})
        } else {
            let mut value = json!({"search":self.search,"pageSize":self.page_size.get()});
            if let Some(filter) = &self.filter {
                value["filter"] = Value::String(filter.clone());
            }
            value
        };
        Request::fabric(Method::Post, &["catalog", "search"])?.with_json(&payload)
    }
}

/// One source page for an interactive caller that owns prompts/output/lifecycle.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchPage {
    /// Rows in service order, without local type filtering.
    pub rows: Vec<Value>,
    /// Opaque next token. It must not become a URL or credential selector.
    pub next_token: Option<String>,
}

/// Source hidden read collections; no generic workspace mutation ownership.
#[derive(Clone, Debug)]
pub enum HiddenCollection {
    /// Tenant connection catalog.
    Connections,
    /// Tenant gateway catalog.
    Gateways,
    /// Admin domains; payload uses `domains`, not `value`.
    Domains,
    /// Connection type metadata.
    ConnectionTypes,
    /// Workspace Spark pools.
    SparkPools(String),
    /// Workspace managed private endpoints.
    ManagedPrivateEndpoints(String),
    /// Shares belong to an item, not a fabricated workspace-wide endpoint.
    ItemExternalDataShares {
        /// Workspace ID.
        workspace: String,
        /// Parent item ID.
        item: String,
    },
    /// Admin domain workspace membership, read-only.
    DomainWorkspaces(String),
}

impl HiddenCollection {
    /// Exact pinned source route plus its resource-specific collection field.
    pub fn request(&self) -> Result<(Request, &'static str), ApiError> {
        let segments = match self {
            Self::Connections => vec!["connections"],
            Self::Gateways => vec!["gateways"],
            Self::Domains => vec!["admin", "domains"],
            Self::ConnectionTypes => vec!["connections", "supportedConnectionTypes"],
            Self::SparkPools(ws) => vec!["workspaces", ws, "spark", "pools"],
            Self::ManagedPrivateEndpoints(ws) => vec!["workspaces", ws, "managedPrivateEndpoints"],
            Self::ItemExternalDataShares { workspace, item } => {
                vec!["workspaces", workspace, "items", item, "externalDataShares"]
            }
            Self::DomainWorkspaces(id) => vec!["admin", "domains", id, "workspaces"],
        };
        Ok((
            Request::fabric(Method::Get, &segments)?,
            if matches!(self, Self::Domains) {
                "domains"
            } else {
                "value"
            },
        ))
    }
}

/// Caller-composed catalog client, with no HTTP/facade/identity/CLI dependency.
#[derive(Clone, Debug)]
pub enum HiddenResource {
    /// Tenant-level connection metadata.
    Connection(String),
    /// Tenant-level gateway metadata.
    Gateway(String),
    /// Administrator domain metadata.
    Domain(String),
    /// Workspace-scoped Spark pool metadata.
    SparkPool {
        /// Parent workspace.
        workspace: String,
        /// Pool identifier.
        id: String,
    },
    /// Workspace-scoped managed private endpoint metadata.
    ManagedPrivateEndpoint {
        /// Parent workspace.
        workspace: String,
        /// Endpoint identifier.
        id: String,
    },
    /// External share metadata under its actual parent item.
    ExternalDataShare {
        /// Parent workspace.
        workspace: String,
        /// Parent item.
        item: String,
        /// Share identifier.
        id: String,
    },
}

impl HiddenResource {
    /// Builds the source singleton route; no generic hidden collection is guessed.
    pub fn request(&self) -> Result<Request, ApiError> {
        let segments = match self {
            Self::Connection(id) => vec!["connections", id],
            Self::Gateway(id) => vec!["gateways", id],
            Self::Domain(id) => vec!["admin", "domains", id],
            Self::SparkPool { workspace, id } => {
                vec!["workspaces", workspace, "spark", "pools", id]
            }
            Self::ManagedPrivateEndpoint { workspace, id } => {
                vec!["workspaces", workspace, "managedPrivateEndpoints", id]
            }
            Self::ExternalDataShare {
                workspace,
                item,
                id,
            } => vec![
                "workspaces",
                workspace,
                "items",
                item,
                "externalDataShares",
                id,
            ],
        };
        Request::fabric(Method::Get, &segments)
    }
}

/// Caller-composed catalog client, with no HTTP/facade/identity/CLI dependency.
pub struct CatalogClient {
    api: ApiClient,
}

impl CatalogClient {
    /// Binds the existing injected API client without I/O.
    pub fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Opts read execution into an explicitly command-owned dispatch trace.
    /// Queries are observed, never copied into subsequent requests implicitly.
    pub fn with_query_dispatch_trace(mut self, trace: fabric_sdk_api::QueryDispatchTrace) -> Self {
        self.api = self.api.with_query_dispatch_trace(trace);
        self
    }

    /// Lists supported connection types using the source's optional gatewayId
    /// query, not a gateway metadata or workspace-scoped substitute endpoint.
    pub async fn supported_connection_types(
        &self,
        gateway_id: Option<&str>,
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Collection, CollectionError> {
        let request = HiddenCollection::ConnectionTypes
            .request()
            .and_then(|(request, _)| match gateway_id {
                Some(id) => request.with_query(&[("gatewayId".into(), id.into())]),
                None => Ok(request),
            })
            .map_err(|error| CollectionError {
                error,
                partial: Box::default(),
            })?;
        self.api
            .collect(request, "value", Paging::Query, limits, cancelled)
            .await
    }

    /// Gets actual hidden resource metadata with shared execution and explicit
    /// byte/status/cancellation failures. Missing fields are not fabricated.
    pub async fn get_hidden(
        &self,
        resource: &HiddenResource,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Value, ApiError> {
        let response = self
            .api
            .send(&resource.request()?, limit, cancelled)
            .await?;
        require_status(&response, &[200])?;
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidJson)?;
        if !value.is_object() {
            return Err(ApiError::InvalidResponse);
        }
        if let Some(token) = value
            .get("continuationToken")
            .filter(|token| !token.is_null())
            && token.as_str() != Some("")
        {
            return Err(ApiError::Continuation);
        }
        Ok(value)
    }

    /// Fetches all command-line results with the reusable bounded API pager.
    /// Continuations contain only token/pageSize, never the original search/filter.
    pub async fn search(
        &self,
        options: &SearchOptions,
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Collection, CollectionError> {
        let request = options.request(None).map_err(|error| CollectionError {
            error,
            partial: Box::default(),
        })?;
        self.api
            .collect_with(
                request,
                "value",
                Paging::Catalog {
                    page_size: options.page_size.get(),
                },
                limits,
                cancelled,
                search_payload,
            )
            .await
    }

    /// Reads one actual catalog page for a caller-owned interactive session.
    /// The caller must maintain page/row/cycle bounds across successive calls.
    pub async fn search_page(
        &self,
        options: &SearchOptions,
        continuation: Option<&str>,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<SearchPage, ApiError> {
        let response = self
            .api
            .send(&options.request(continuation)?, limit, cancelled)
            .await?;
        let value = search_payload(&response)?;
        let rows = value
            .get("value")
            .and_then(Value::as_array)
            .ok_or(ApiError::InvalidResponse)?;
        if rows.iter().any(|r| !r.is_object()) {
            return Err(ApiError::InvalidResponse);
        }
        let next_token = match value.get("continuationToken") {
            None | Some(Value::Null) => None,
            Some(Value::String(token)) => (!token.is_empty()).then(|| token.clone()),
            _ => return Err(ApiError::Continuation),
        };
        Ok(SearchPage {
            rows: rows.clone(),
            next_token,
        })
    }

    /// Lists a hidden read collection with shared cancellation, bounds and paging.
    pub async fn list_hidden(
        &self,
        collection: &HiddenCollection,
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Collection, CollectionError> {
        self.list_hidden_with_query(collection, &[], limits, cancelled)
            .await
    }

    /// Lists with explicit initial query pairs and optional caller-owned dispatch
    /// evidence. This does not mutate the pairs or install persistent query state;
    /// the compatibility host decides whether source command state is reused.
    pub async fn list_hidden_with_query(
        &self,
        collection: &HiddenCollection,
        query: &[(String, String)],
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Collection, CollectionError> {
        let (request, field) = collection
            .request()
            .and_then(|(request, field)| Ok((request.with_query(query)?, field)))
            .map_err(|error| CollectionError {
                error,
                partial: Box::default(),
            })?;
        self.api
            .collect(request, field, Paging::Query, limits, cancelled)
            .await
    }

    /// Reads workspace identity metadata from the real workspace endpoint.
    /// It does not invent a managed-identities collection or perform identity login.
    pub async fn workspace_identity(
        &self,
        workspace: &str,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<Value>, ApiError> {
        let response = self
            .api
            .send(
                &Request::fabric(Method::Get, &["workspaces", workspace])?,
                limit,
                cancelled,
            )
            .await?;
        require_status(&response, &[200])?;
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidResponse)?;
        if !value.is_object() {
            return Err(ApiError::InvalidResponse);
        }
        match value.get("workspaceIdentity") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(identity)) if identity.is_empty() => Ok(None),
            Some(Value::Object(identity))
                if identity
                    .get("servicePrincipalId")
                    .is_some_and(Value::is_string) =>
            {
                Ok(Some(Value::Object(identity.clone())))
            }
            _ => Err(ApiError::InvalidResponse),
        }
    }
}
