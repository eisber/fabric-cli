// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned source contracts, not assertions about deployed service reachability.

use crate::runtime::NativeMethod;

/// JSON source revision used by every contract in this module.
pub const SOURCE_REVISION: &str = "10b9db3a20ca1971f8d8a5fff981ff65260b022d";

/// An explicitly selected service contract; native extensions are not Fabric capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceProfile {
    /// Wildfire native JSON, serialized jobs and described multipart routes.
    NativeWildfire,
    /// Only the twenty modeled Fabric gateway JSON method/path pairs.
    FabricJson,
}

/// Implementation/qualification distinction visible to direct SDK consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Maturity {
    /// Implemented against pinned source; deployed reachability has not been validated.
    SourceModeledDeploymentUnverified,
    /// Inventoried native multipart route, blocked by the shared JSON request seam.
    MultipartTransportRequired,
}

/// Declared request representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestEncoding {
    /// JSON object or no body.
    Json,
    /// One binary multipart part; cannot be represented by `NativeRequest`.
    Multipart,
}

/// Exact source method/path/schema identity. Placeholders are not accepted as caller URLs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteContract {
    /// Native HTTP method.
    pub method: NativeMethod,
    /// Relative source path template.
    pub path: &'static str,
    /// Source SDK member or modeled OpenAPI operation ID.
    pub source_id: &'static str,
    /// Source response model (opaque models preserve all JSON fields).
    pub response_schema: &'static str,
    /// Body representation.
    pub encoding: RequestEncoding,
}

impl RouteContract {
    /// Report source coverage without claiming deployed/native qualification.
    pub const fn maturity(&self) -> Maturity {
        match self.encoding {
            RequestEncoding::Json => Maturity::SourceModeledDeploymentUnverified,
            RequestEncoding::Multipart => Maturity::MultipartTransportRequired,
        }
    }
}

macro_rules! route {
    ($method:ident, $path:literal, $id:literal, $response:literal) => {
        RouteContract {
            method: NativeMethod::$method,
            path: $path,
            source_id: $id,
            response_schema: $response,
            encoding: RequestEncoding::Json,
        }
    };
    (upload $path:literal, $id:literal) => {
        RouteContract {
            method: NativeMethod::Post,
            path: $path,
            source_id: $id,
            response_schema: "Acknowledgement",
            encoding: RequestEncoding::Multipart,
        }
    };
}

/// All 49 native source pairs, including the three explicitly unsupported multipart transports.
///
/// Source: `docs/reference/native-rest.md`, `python/src/microsoft/fabric/livy/client.py`.
pub const NATIVE_ROUTES: &[RouteContract] = &[
    route!(Get, "/version", "LivyClient.version", "LivyModel"),
    route!(Get, "/sessions", "SessionsClient.list", "SessionList"),
    route!(Post, "/sessions", "SessionsClient.create", "Session"),
    route!(Get, "/sessions/{id}", "SessionClient.get", "Session"),
    route!(Delete, "/sessions/{id}", "SessionClient.delete", "Acknowledgement"),
    route!(Get, "/sessions/{id}/state", "SessionClient.get_state", "Session"),
    route!(Get, "/sessions/{id}/log", "SessionClient.logs", "Logs"),
    route!(Put, "/sessions/{id}/reset-timeout", "SessionClient.reset_timeout", "Acknowledgement"),
    route!(Post, "/sessions/{id}/stop", "SessionClient.stop", "Acknowledgement"),
    route!(Post, "/sessions/{id}/interrupt", "SessionClient.interrupt", "CancellationResult"),
    route!(Post, "/sessions/{id}/connect", "SessionClient.connect", "Session"),
    route!(Get, "/sessions/{id}/timeout", "SessionClient.get_timeout", "Timeout"),
    route!(Put, "/sessions/{id}/timeout", "SessionClient.set_timeout", "Timeout"),
    route!(Post, "/sessions/{id}/completion", "SessionClient.completion", "Completion"),
    route!(Post, "/sessions/{id}/inspect", "SessionClient.inspect", "Inspection"),
    route!(Post, "/sessions/{id}/debug", "SessionClient.debug", "LivyModel"),
    route!(Post, "/sessions/{id}/add-jar", "SessionClient.add_jar", "Acknowledgement"),
    route!(Post, "/sessions/{id}/add-pyfile", "SessionClient.add_pyfile", "Acknowledgement"),
    route!(Post, "/sessions/{id}/add-file", "SessionClient.add_file", "Acknowledgement"),
    route!(upload "/sessions/{id}/upload-jar", "SessionClient.upload_jar"),
    route!(upload "/sessions/{id}/upload-pyfile", "SessionClient.upload_pyfile"),
    route!(upload "/sessions/{id}/upload-file", "SessionClient.upload_file"),
    route!(Post, "/sessions/{id}/submit-job", "JobsClient.submit", "Job"),
    route!(Post, "/sessions/{id}/run-job", "JobsClient.run", "Job"),
    route!(Get, "/sessions/{id}/jobs/{jobId}", "JobsClient.get", "Job"),
    route!(Post, "/sessions/{id}/jobs/{jobId}/cancel", "JobsClient.cancel", "Acknowledgement"),
    route!(Get, "/sessions/{id}/statements", "StatementsClient.list", "StatementList"),
    route!(Post, "/sessions/{id}/statements", "StatementsClient.submit", "Statement"),
    route!(Get, "/sessions/{id}/statements/{statementId}", "StatementClient.get", "Statement"),
    route!(Post, "/sessions/{id}/statements/{statementId}/cancel", "StatementClient.cancel", "CancellationResult"),
    route!(Get, "/sessions/{id}/repls", "ReplsClient.list", "SessionList"),
    route!(Put, "/sessions/{id}/repls/{replId}", "ReplsClient.create", "Session"),
    route!(Get, "/sessions/{id}/repls/{replId}", "ReplClient.get", "Session"),
    route!(Delete, "/sessions/{id}/repls/{replId}", "ReplClient.delete", "Acknowledgement"),
    route!(Post, "/sessions/{id}/repls/{replId}/stop", "ReplClient.stop", "Acknowledgement"),
    route!(Put, "/sessions/{id}/repls/{replId}/reset-timeout", "ReplClient.reset_timeout", "CancellationResult"),
    route!(Get, "/sessions/{id}/repls/{replId}/timeout", "ReplClient.get_timeout", "Timeout"),
    route!(Put, "/sessions/{id}/repls/{replId}/timeout", "ReplClient.set_timeout", "Timeout"),
    route!(Get, "/sessions/{id}/repls/{replId}/statements", "StatementsClient.list", "StatementList"),
    route!(Post, "/sessions/{id}/repls/{replId}/statements", "StatementsClient.submit", "Statement"),
    route!(Get, "/sessions/{id}/repls/{replId}/statements/{statementId}", "StatementClient.get", "Statement"),
    route!(Post, "/sessions/{id}/repls/{replId}/statements/{statementId}/cancel", "StatementClient.cancel", "CancellationResult"),
    route!(Get, "/batches", "BatchesClient.list", "BatchList"),
    route!(Post, "/batches", "BatchesClient.create", "Batch"),
    route!(Get, "/batches/{id}", "BatchesClient.get", "Batch"),
    route!(Delete, "/batches/{id}", "BatchesClient.delete", "Acknowledgement"),
    route!(Get, "/batches/{id}/state", "BatchesClient.get_state", "Batch"),
    route!(Get, "/batches/{id}/log", "BatchesClient.logs", "Logs"),
    route!(Put, "/batches/{id}/reset-timeout", "BatchesClient.reset_timeout", "Acknowledgement"),
];

/// All 20 Fabric operations from `contracts/rest/generated/openapi.yaml`.
///
/// The JSON contract intentionally excludes native REPL management, logs, jobs and tooling.
pub const FABRIC_ROUTES: &[RouteContract] = &[
    route!(Get, "/batches", "listBatches", "ActivityList"),
    route!(Post, "/batches", "createBatch", "Batch"),
    route!(Get, "/batches/{id}", "getBatch", "Batch"),
    route!(Delete, "/batches/{id}", "deleteBatch", "Acknowledgement"),
    route!(Post, "/highConcurrencySessions", "acquireHighConcurrencySession", "Session"),
    route!(Get, "/highConcurrencySessions/{id}", "getHighConcurrencySession", "Session"),
    route!(Delete, "/highConcurrencySessions/{id}", "deleteHighConcurrencySession", "Acknowledgement"),
    route!(Get, "/highConcurrencySessions/{id}/repls/{replId}/statements", "listHighConcurrencyStatements", "ActivityList"),
    route!(Post, "/highConcurrencySessions/{id}/repls/{replId}/statements", "submitHighConcurrencyStatement", "Statement"),
    route!(Get, "/highConcurrencySessions/{id}/repls/{replId}/statements/{statementId}", "getHighConcurrencyStatement", "Statement"),
    route!(Post, "/highConcurrencySessions/{id}/repls/{replId}/statements/{statementId}/cancel", "cancelHighConcurrencyStatement", "CancellationResult"),
    route!(Get, "/sessions", "listSessions", "ActivityList"),
    route!(Post, "/sessions", "createSession", "Session"),
    route!(Get, "/sessions/{id}", "getSession", "Session"),
    route!(Delete, "/sessions/{id}", "deleteSession", "Acknowledgement"),
    route!(Post, "/sessions/{id}/reset-timeout", "resetSessionTimeout", "Session"),
    route!(Get, "/sessions/{id}/statements", "listSessionStatements", "ActivityList"),
    route!(Post, "/sessions/{id}/statements", "submitSessionStatement", "Statement"),
    route!(Get, "/sessions/{id}/statements/{statementId}", "getSessionStatement", "Statement"),
    route!(Post, "/sessions/{id}/statements/{statementId}/cancel", "cancelSessionStatement", "CancellationResult"),
];

impl ServiceProfile {
    /// Enumerate the finite admitted source surface; no probing or network access.
    pub const fn routes(self) -> &'static [RouteContract] {
        match self {
            Self::NativeWildfire => NATIVE_ROUTES,
            Self::FabricJson => FABRIC_ROUTES,
        }
    }
}
