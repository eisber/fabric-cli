// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Physical Lakehouse schema over the existing TABLE identity and resolver.
//!
//! The caller owns the read session, HTTP executor, runtime and cancellation.
//! Only the generic OneLake read adapter is mounted; raw TABLE reads and loads
//! are unavailable. No settings, navigation store or identity is discovered here.

use crate::{
    session::ReadSession,
    tables::{Targets, table_client},
    v2_discovery::{
        DataSchemaProvider, DataSchemaRequest, DeltaDataSchemaProvider, DiscoveryFailure,
        DiscoveryFuture,
    },
};
use fabric_cli_tables::TableFailure;
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_onelake::OneLakeExecutor;
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_tables::{
    LoadOutcome, TableError, TableErrorKind, TableExecutor, TableFuture, TableLimits, TableRequest,
    TableResponse, schema_document::DataSchemaDocument,
};
use std::{num::NonZeroUsize, sync::Arc};

/// Caller-injected production data-schema provider, with finite TABLE bounds.
pub struct NativeSchemaProvider<'a, 'session> {
    /// Existing selected Fabric identity, metadata context and clock.
    pub reads: &'a ReadSession<'session>,
    /// Existing bounded HTTP executor, shared by resolution and storage reads.
    pub http: &'a HttpExecutor,
    /// Existing SDK request/log/response limits and serialized output cap.
    pub limits: TableLimits,
}

impl DataSchemaProvider for NativeSchemaProvider<'_, '_> {
    fn max_output_bytes(&self) -> NonZeroUsize {
        self.limits.max_response_bytes
    }

    fn schema<'a>(
        &'a self,
        request: &'a DataSchemaRequest,
        cancelled: DiscoveryFuture<'a, ()>,
    ) -> DiscoveryFuture<'a, Result<DataSchemaDocument, DiscoveryFailure>> {
        Box::pin(async move {
            let sdk = table_client(self.reads).map_err(|error| match error {
                TableFailure::Command(error) | TableFailure::Options(error) => {
                    DiscoveryFailure::Resolver(error)
                }
                TableFailure::Sdk(error) => DiscoveryFailure::Table(error),
            })?;
            let resolver = Targets::physical(self.reads, self.http);
            let executor = SchemaReads(OneLakeHttpExecutor::new(Arc::new(self.http.clone())));
            DeltaDataSchemaProvider {
                sdk: &sdk,
                executor: &executor,
                resolver: &resolver,
                limits: self.limits,
            }
            .schema(request, cancelled)
            .await
        })
    }
}

struct SchemaReads(OneLakeHttpExecutor);

impl TableExecutor for SchemaReads {
    fn onelake(&self) -> Option<&dyn OneLakeExecutor> {
        Some(&self.0)
    }

    fn read<'a>(
        &'a self,
        _: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async { Err(TableError::new(TableErrorKind::InvalidInput)) })
    }

    fn load<'a>(
        &'a self,
        _: TableRequest<'a>,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async { Err(TableError::new(TableErrorKind::InvalidInput)) })
    }
}
