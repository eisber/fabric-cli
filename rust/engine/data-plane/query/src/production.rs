// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{EngineBinding, QueryError, QueryErrorKind};
use azure_core::{credentials::TokenCredential, http::HttpClient};
use fabric_sdk_http::ExecutionTimer;
use std::{fmt, sync::Arc};

const PUBLIC_KUSTO_SCOPE: &str = "https://api.kusto.windows.net/.default";

/// Caller-owned credential and explicit Kusto identity selection.
///
/// The caller must configure this provider for the stated account and tenant.
/// Selectors cannot prove an opaque token's claims. No provider is discovered,
/// prompted, reconfigured or replaced, and no token is obtained at construction.
#[derive(Clone)]
pub struct KustoCredentialContext {
    credential: Arc<dyn TokenCredential>,
    account: String,
    tenant: String,
    audience_scope: String,
}

impl KustoCredentialContext {
    /// Bind an injected credential to non-secret identity selectors and scope.
    ///
    /// This adapter supports the literal public-cloud Kusto audience only.
    /// Fabric REST, ARM, SQL, arbitrary cluster-derived and sovereign scopes
    /// are unsupported; an endpoint never selects or repairs an audience.
    ///
    /// # Errors
    /// Rejects unsupported scopes and invalid selectors without credential I/O.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        account: &str,
        tenant: &str,
        audience_scope: &str,
    ) -> Result<Self, QueryError> {
        if audience_scope != PUBLIC_KUSTO_SCOPE {
            return Err(QueryError::new(QueryErrorKind::UnsupportedCapability));
        }
        for value in [account, tenant] {
            if value.trim().is_empty()
                || value.trim() != value
                || value.len() > 4096
                || value.chars().any(char::is_control)
            {
                return Err(QueryError::new(QueryErrorKind::InvalidBinding));
            }
        }
        Ok(Self {
            credential,
            account: account.into(),
            tenant: tenant.into(),
            audience_scope: audience_scope.into(),
        })
    }
}

impl fmt::Debug for KustoCredentialContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KustoCredentialContext(<explicit>)")
    }
}

/// Fixed-policy Kusto v2 query executor over the caller's shared HTTP/runtime.
///
/// The injected HTTP client must stream, disable redirects and retries, and
/// release local I/O on drop. Construction validates structure and selection,
/// not live service permissions or tenant read-policy enforcement.
#[derive(Clone)]
pub struct KustoQueryExecutor {
    binding: EngineBinding,
    credentials: KustoCredentialContext,
    client: Arc<dyn HttpClient>,
    timer: Arc<dyn ExecutionTimer>,
}

impl KustoQueryExecutor {
    /// Compose an exact engine binding, credential and existing HTTP/time stack.
    ///
    /// No networking, token acquisition, global runtime, identity fallback or
    /// transport configuration occurs. Every binding field is retained; the
    /// credential account, tenant and literal audience must match exactly.
    ///
    /// # Errors
    /// Rejects an unequal selection before acquiring credentials or opening I/O.
    pub fn new(
        binding: EngineBinding,
        credentials: KustoCredentialContext,
        client: Arc<dyn HttpClient>,
        timer: Arc<dyn ExecutionTimer>,
    ) -> Result<Self, QueryError> {
        if binding.audience_scope() != PUBLIC_KUSTO_SCOPE {
            return Err(QueryError::new(QueryErrorKind::UnsupportedCapability));
        }
        if binding.account() != credentials.account
            || binding.tenant() != credentials.tenant
            || binding.audience_scope() != credentials.audience_scope
        {
            return Err(QueryError::new(QueryErrorKind::InvalidBinding));
        }
        Ok(Self {
            binding,
            credentials,
            client,
            timer,
        })
    }
}

impl fmt::Debug for KustoQueryExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KustoQueryExecutor(<exact-binding-and-injected-stack>)")
    }
}
