// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{Origin, Request};
use azure_core::http::{Method, Url};
use std::{
    borrow::Cow,
    fmt,
    sync::{Arc, Mutex},
};

/// The last validated logical request handed to the caller's executor after
/// successful audience/capability checks and credential acquisition.
///
/// This is dispatch evidence, not transport delivery, service acceptance, or
/// success. GET retries remain inside the existing executor. No response token,
/// credential, header or body is retained, and Debug omits the URL/query.
#[derive(Clone, PartialEq)]
pub struct DispatchedQuery {
    origin: Origin,
    method: Method,
    url: Url,
}

impl DispatchedQuery {
    /// Explicit request origin, never inferred as a credential selector.
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    /// Method actually handed to the executor.
    pub const fn method(&self) -> Method {
        self.method
    }

    /// Original dispatched URL. Opaque continuation values may be sensitive;
    /// consumers must not log it or treat it as a new authorized destination.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Once-encoded dispatched query, preserving absence versus an empty query.
    pub fn encoded_query(&self) -> Option<&str> {
        self.url.query()
    }

    /// Ordered decoded query pairs; opaque values are decoded exactly once.
    pub fn query_pairs(&self) -> impl Iterator<Item = (Cow<'_, str>, Cow<'_, str>)> {
        self.url.query_pairs()
    }
}

impl fmt::Debug for DispatchedQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DispatchedQuery")
            .field("origin", &self.origin)
            .field("method", &self.method)
            .field("has_query", &self.url.query().is_some())
            .finish_non_exhaustive()
    }
}

/// Opt-in command-owned evidence retaining only the last executor dispatch.
///
/// Create a fresh trace per command/identity scope, or explicitly clear it.
/// Clones share this one snapshot; no ambient state, accumulated history or
/// automatic query carry is installed. Queries retain existing request/paging
/// bounds. A failure/cancellation before dispatch leaves the prior snapshot
/// unchanged; an error after dispatch retains the failing request's query.
/// Use one trace for a serial compatibility command, not for inferring an order
/// between concurrent requests. Clearing while work is active is not cancellation.
#[derive(Clone, Default)]
pub struct QueryDispatchTrace {
    last: Arc<Mutex<Option<DispatchedQuery>>>,
}

impl QueryDispatchTrace {
    /// Returns an owned snapshot, including on collection error or cancellation.
    /// This is not `Collection::next_token`, which is the next unconsumed token.
    pub fn last_dispatch(&self) -> Option<DispatchedQuery> {
        self.last.lock().expect("query dispatch trace lock").clone()
    }

    /// Explicit caller reset. It does not cancel work, change requests or clear
    /// another independently constructed trace.
    pub fn clear(&self) {
        *self.last.lock().expect("query dispatch trace lock") = None;
    }

    pub(crate) fn record(&self, request: &Request) {
        *self.last.lock().expect("query dispatch trace lock") = Some(DispatchedQuery {
            origin: request.origin(),
            method: request.method(),
            url: request.url().clone(),
        });
    }
}

impl fmt::Debug for QueryDispatchTrace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueryDispatchTrace")
            .field("last", &self.last_dispatch())
            .finish()
    }
}
