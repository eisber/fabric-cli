// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{QueryError, QueryErrorKind};
use azure_core::http::Url;
use fabric_sdk::runtime::{NativeEndpoint, NativeStreamExecutor};
use fabric_v2_contracts::ItemIdentity;
use serde::Serialize;
use std::fmt;

/// An exact resolved engine context shared with write-capable Execute consumers.
///
/// A binding is target metadata, not permission or a token-claim verification.
/// This first protocol is Kusto; management request construction/admission belongs
/// to Execute. It does not use query text to select a database, language or route.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineBinding {
    id: String,
    version: String,
    owner: ItemIdentity,
    #[serde(skip)]
    endpoint: NativeEndpoint,
    endpoint_uri: String,
    database: String,
    audience_scope: String,
    account: String,
    tenant: String,
}

impl EngineBinding {
    /// Retain one caller-resolved Kusto endpoint, database and identity context.
    ///
    /// `endpoint` is an exact HTTPS origin, not a query URI or ingestion endpoint.
    /// `audience_scope` is explicitly selected, never derived from the hostname.
    /// The caller owns the resolver/permission evidence and credential binding.
    ///
    /// # Errors
    /// Rejects unsafe or unbounded metadata without credentials or networking.
    #[allow(
        clippy::too_many_arguments,
        reason = "Keeps the exact resolved engine context atomic."
    )]
    pub fn kusto(
        id: &str,
        version: &str,
        owner: ItemIdentity,
        endpoint: Url,
        database: &str,
        audience_scope: &str,
        account: &str,
        tenant: &str,
    ) -> Result<Self, QueryError> {
        for value in [id, version, database, audience_scope, account, tenant] {
            if value.trim().is_empty()
                || value.trim() != value
                || value.len() > 4096
                || value.chars().any(char::is_control)
            {
                return Err(QueryError::new(QueryErrorKind::InvalidBinding));
            }
        }
        let scope_base = audience_scope
            .strip_suffix("/.default")
            .ok_or_else(|| QueryError::new(QueryErrorKind::InvalidBinding))?;
        let scope =
            Url::parse(scope_base).map_err(|_| QueryError::new(QueryErrorKind::InvalidBinding))?;
        if endpoint.scheme() != "https"
            || endpoint.path() != "/"
            || endpoint.port_or_known_default() != Some(443)
            || scope.scheme() != "https"
            || scope.host_str().is_none()
            || !scope.username().is_empty()
            || scope.password().is_some()
            || scope.query().is_some()
            || scope.fragment().is_some()
            || scope.port_or_known_default() != Some(443)
        {
            return Err(QueryError::new(QueryErrorKind::InvalidBinding));
        }
        let endpoint_uri = endpoint.to_string();
        let endpoint = NativeEndpoint::new(endpoint)
            .map_err(|_| QueryError::new(QueryErrorKind::InvalidBinding))?;
        Ok(Self {
            id: id.into(),
            version: version.into(),
            owner,
            endpoint,
            endpoint_uri,
            database: database.into(),
            audience_scope: audience_scope.into(),
            account: account.into(),
            tenant: tenant.into(),
        })
    }

    /// Resolver-owned stable binding identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Resolver-owned binding contract version, distinct from response version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Exact Fabric owner, not the first available workspace or database.
    pub fn owner(&self) -> &ItemIdentity {
        &self.owner
    }

    /// Exact endpoint for constructing separately authorized native operations.
    pub fn endpoint(&self) -> &NativeEndpoint {
        &self.endpoint
    }

    /// Exact database payload value, never a display-name fallback.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Explicit scope required of the injected executor's credential context.
    pub fn audience_scope(&self) -> &str {
        &self.audience_scope
    }

    /// Explicit non-secret account selector; not a discovered credential.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Explicit non-secret tenant selector.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
}

impl fmt::Debug for EngineBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EngineBinding(<explicit>)")
    }
}

/// Honest capability state of the injected engine adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryCapability {
    /// An offline scripted executor with no service or credential access.
    ScriptedOffline,
    /// Native read enforcement/audience wiring has not been verified.
    ///
    /// This state always rejects Query before opening a stream. There is no
    /// production-enable boolean or syntax-based safety escape hatch.
    UnverifiedProduction,
}

/// Borrowed native streaming executor bound to one exact resolved engine context.
///
/// Implementations own their existing credential/HTTP/time stack. They must
/// match every binding field, honor the absolute deadline/byte limits, disable
/// redirects and automatic retries, and release local I/O when dropped.
/// Returning `ScriptedOffline` is a promise of offline execution, not a mechanism
/// for granting production permission. No production implementation is supplied.
pub trait EngineExecutor: NativeStreamExecutor {
    /// Exact context used by this adapter, checked before submission.
    fn binding(&self) -> &EngineBinding;

    /// Native production support defaults to unavailable, not optimistic success.
    fn query_capability(&self) -> QueryCapability {
        QueryCapability::UnverifiedProduction
    }
}
