// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{DatabaseKind, KqlDatabase, KqlError, KqlErrorKind, Observed};
use crate::{Audience, RequestContext, ServiceOrigin, runtime::NativeEndpoint, v2::ItemIdentity};
use azure_core::http::Url;
use std::fmt;

/// Explicit native database selector; there is no name/ID retry or fallback.
pub enum DatabaseSelector<'a> {
    /// Use a caller-confirmed native name, not an arbitrary cross-target override.
    ConfirmedName(&'a str),
    /// Explicitly select the documented display-name mapping for this metadata.
    MetadataDisplayName,
}

/// Explicit OAuth resource choice, independent of Fabric and ingestion audiences.
///
/// Choosing a resource records caller intent, not provider/cloud equivalence or
/// consent. Consumers must verify this binding and inject a conforming credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KustoResource {
    /// Explicit public Kusto resource documented by the HTTPS authentication API.
    PublicService,
    /// Explicit cluster resource documented by Fabric's deployment example.
    QueryOrigin,
}

/// A pure handoff descriptor, not an authenticated or read-only execution client.
#[derive(Clone)]
pub struct KustoContext {
    identity: ItemIdentity,
    parent_eventhouse: ItemIdentity,
    database: String,
    database_kind: DatabaseKind,
    query: NativeEndpoint,
    ingestion: Observed<NativeEndpoint>,
    resource: Url,
    scope: String,
    metadata_context: RequestContext,
}

impl KustoContext {
    /// Concrete owner checked against the selected metadata request.
    pub fn identity(&self) -> &ItemIdentity {
        &self.identity
    }
    /// Independently identified parent Eventhouse in the same workspace.
    pub fn parent_eventhouse(&self) -> &ItemIdentity {
        &self.parent_eventhouse
    }
    /// Explicitly selected native database name.
    pub fn database(&self) -> &str {
        &self.database
    }
    /// Native database kind; neither variant implies permission to execute writes.
    pub fn database_kind(&self) -> &DatabaseKind {
        &self.database_kind
    }
    /// Validated metadata-supplied query origin, with no query/control route.
    pub fn query_endpoint(&self) -> &NativeEndpoint {
        &self.query
    }
    /// Independently validated observed ingestion origin; never synthesized.
    pub fn ingestion_endpoint(&self) -> &Observed<NativeEndpoint> {
        &self.ingestion
    }
    /// Explicit Kusto resource, never the Fabric metadata resource.
    pub fn resource(&self) -> &Url {
        &self.resource
    }
    /// Scope for the caller's explicitly chosen resource; no token is acquired.
    pub fn scope(&self) -> &str {
        &self.scope
    }
    /// Original metadata context and credential Arc, with no audience mutation.
    ///
    /// This remains a Fabric context. Query owners borrow its explicit account,
    /// tenant and credential while binding Kusto authentication independently.
    pub fn metadata_context(&self) -> &RequestContext {
        &self.metadata_context
    }
}

impl fmt::Debug for KustoContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KustoContext(<explicit descriptor; authorization unverified>)")
    }
}

impl KqlDatabase {
    /// Resolves observed metadata using explicit database and OAuth resource choices.
    ///
    /// No metadata URI aliases, endpoint probes, query/ingestion substitution,
    /// alternate identities or synthesized parent IDs are permitted. Unknown
    /// native kinds cannot establish this executable-consumer descriptor.
    ///
    /// # Errors
    /// Returns typed missing/invalid binding or context failures without I/O.
    pub fn kusto_context(
        &self,
        database: DatabaseSelector<'_>,
        resource: KustoResource,
    ) -> Result<KustoContext, KqlError> {
        validate_context(&self.context)?;
        let Observed::Value(properties) = self.properties() else {
            return Err(KqlError::new(KqlErrorKind::MissingBinding));
        };
        let Observed::Value(parent) = &properties.parent_eventhouse_item_id else {
            return Err(KqlError::new(KqlErrorKind::MissingBinding));
        };
        let parent_eventhouse = checked_identity(self.identity().workspace_id(), parent)?;
        if parent.eq_ignore_ascii_case(self.identity().item_id()) {
            return Err(KqlError::new(KqlErrorKind::TargetMismatch));
        }
        let Observed::Value(kind @ (DatabaseKind::ReadWrite | DatabaseKind::Shortcut)) =
            &properties.database_type
        else {
            return Err(KqlError::new(KqlErrorKind::MissingBinding));
        };
        let Observed::Value(query_uri) = &properties.query_service_uri else {
            return Err(KqlError::new(KqlErrorKind::MissingBinding));
        };
        let query = endpoint(query_uri, false)?;
        let ingestion = match &properties.ingestion_service_uri {
            Observed::Missing => Observed::Missing,
            Observed::Null => Observed::Null,
            Observed::Value(uri) => Observed::Value(endpoint(uri, true)?),
        };
        let database = match database {
            DatabaseSelector::ConfirmedName(name) => name,
            DatabaseSelector::MetadataDisplayName => self.display_name(),
        };
        if database.trim().is_empty()
            || database.len() > 1024
            || database.chars().any(char::is_control)
        {
            return Err(KqlError::new(KqlErrorKind::InvalidInput));
        }
        let resource = match resource {
            KustoResource::PublicService => Url::parse("https://api.kusto.windows.net")
                .map_err(|_| KqlError::new(KqlErrorKind::InvalidInput))?,
            KustoResource::QueryOrigin => query.base().clone(),
        };
        let scope = format!("{}/.default", resource.as_str().trim_end_matches('/'));
        Ok(KustoContext {
            identity: self.identity().clone(),
            parent_eventhouse,
            database: database.into(),
            database_kind: kind.clone(),
            query,
            ingestion,
            resource,
            scope,
            metadata_context: self.context.clone(),
        })
    }
}

pub(super) fn checked_identity(workspace: &str, item: &str) -> Result<ItemIdentity, KqlError> {
    let identity = ItemIdentity::new(workspace, item)
        .map_err(|_| KqlError::new(KqlErrorKind::InvalidInput))?;
    if [workspace, item].contains(&"00000000-0000-0000-0000-000000000000") {
        return Err(KqlError::new(KqlErrorKind::InvalidInput));
    }
    Ok(identity)
}

pub(super) fn validate_context(context: &RequestContext) -> Result<(), KqlError> {
    if context.selection().audience() != Audience::Fabric
        || context.endpoint().origin() != ServiceOrigin::Fabric
    {
        return Err(KqlError::new(KqlErrorKind::InvalidContext));
    }
    Ok(())
}

fn endpoint(uri: &str, ingestion: bool) -> Result<NativeEndpoint, KqlError> {
    let invalid = || KqlError::new(KqlErrorKind::InvalidInput);
    let authority = uri.strip_prefix("https://").ok_or_else(invalid)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.is_empty()
        || !authority.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
    {
        return Err(invalid());
    }
    let prefix = authority
        .strip_suffix(".kusto.fabric.microsoft.com")
        .or_else(|| authority.strip_suffix(".kusto.windows.net"))
        .ok_or_else(invalid)?;
    if prefix.is_empty()
        || prefix.starts_with("ingest-") != ingestion
        || authority.len() > 253
        || authority.split('.').any(|label| {
            label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-')
        })
    {
        return Err(invalid());
    }
    let parsed = Url::parse(uri).map_err(|_| invalid())?;
    NativeEndpoint::new(parsed).map_err(|_| invalid())
}
