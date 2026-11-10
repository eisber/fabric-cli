// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! SDK convenience client; service behavior is delegated to focused packages.

use crate::{CredentialSelection, RequestContext, TokenCredential};
use fabric_sdk_notebooks::{NotebookExecutor, NotebookLimits, NotebookSession};
use std::{fmt, future::Future, sync::Arc};

/// A service client boundary with explicitly injected credentials.
///
/// The provider must be configured for the selected account and tenant.
/// Selection metadata does not validate token claims or bind a provider to a tenant.
#[derive(Clone)]
pub struct Client {
    context: RequestContext,
}

impl Client {
    /// Starts a bounded native item session; performs no I/O until first awaited call.
    ///
    /// Supply a deadline/cancellation future, or `std::future::pending()` to rely
    /// on executor deadlines and dropping the returned operation future.
    pub fn notebook_session<'a>(
        &'a self,
        executor: &'a dyn NotebookExecutor,
        limits: NotebookLimits,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> NotebookSession<'a> {
        NotebookSession::new(self.context(), executor, limits, cancelled)
    }

    /// Stores a caller-provided credential and selection without performing I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self::with_context(RequestContext::new(credential, selection))
    }

    /// Uses explicit request configuration for context-enabled service methods.
    ///
    /// Workspace, item, folder and capacity reads share this binding. Other
    /// facade families retain public defaults until their separate migration.
    pub fn with_context(context: RequestContext) -> Self {
        Self { context }
    }

    /// Returns the immutable caller-owned request configuration.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Returns the injected provider; token acquisition remains explicitly caller-driven.
    pub fn credential(&self) -> &Arc<dyn TokenCredential> {
        self.context.credential()
    }

    /// Returns the explicit selection, without probing the identity provider.
    pub fn selection(&self) -> &CredentialSelection {
        self.context.selection()
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("credential", &"<injected>")
            .field("context", &self.context)
            .finish()
    }
}
