// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, owner-bound JSON byte cursor observation, without automatic I/O.

use super::{BytePage, LivyRequest, LivyResponse, Observation, Statement, StatementSnapshot};
use crate::runtime::NativeRequest;
use serde_json::Value;
use std::{fmt, sync::Arc};

/// Cursor validation failure. The original response/partial output remains caller-owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorError {
    /// Invalid starting offset, size, page budget or byte budget.
    InvalidInput,
    /// Request construction failed.
    InvalidRequest,
    /// Missing/invalid state, output or byte cursor metadata.
    InvalidMetadata,
    /// The page is not a successfully completed statement representation.
    NotSuccessful,
    /// The response belongs to another request/service/acquisition/session/REPL/statement.
    OwnerMismatch,
    /// `next` did not advance beyond the exact requested byte offset.
    NonAdvancing,
    /// Service `next=-2`: retained output is truncated by the provider retrieval limit.
    RetrievalLimit,
    /// A finite page or cumulative response-byte budget was exceeded.
    Limit,
    /// A request is already outstanding, or no request is awaiting observation.
    OutOfOrder,
    /// `next=-1` has already ended this traversal.
    AlreadyComplete,
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Livy byte-page traversal could not continue")
    }
}

impl std::error::Error for CursorError {}

/// Exact provider cursor outcome; fragments are never merged or treated as rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageProgress {
    /// Request the next opaque byte offset.
    More {
        /// Exact returned offset, not computed from text length.
        next_offset: u64,
    },
    /// `next=-1`: traversal of retained output ended, not proof of original row completeness.
    Complete,
}

/// Pure cursor observer tied to one immutable statement owner.
///
/// The caller controls dispatch, absolute deadlines, cancellation and retention
/// of each response. This type never polls, submits code, sleeps, retries or
/// decodes/concatenates result fragments.
pub struct StatementPages {
    statement: Statement,
    offset: u64,
    size: u32,
    max_pages: usize,
    max_total_bytes: usize,
    requested: usize,
    received_bytes: usize,
    pending: Option<NativeRequest>,
    complete: bool,
}

impl StatementPages {
    /// Construct a finite traversal with positive page size/count/byte limits.
    ///
    /// # Errors
    /// Rejects wire-range violations and zero budgets, without any I/O.
    pub fn new(
        statement: Statement,
        offset: u64,
        size: u32,
        max_pages: usize,
        max_total_bytes: usize,
    ) -> Result<Self, CursorError> {
        BytePage::new(Some(offset), Some(size)).map_err(|_| CursorError::InvalidInput)?;
        if size == 0 || max_pages == 0 || max_total_bytes == 0 {
            return Err(CursorError::InvalidInput);
        }
        Ok(Self {
            statement,
            offset,
            size,
            max_pages,
            max_total_bytes,
            requested: 0,
            received_bytes: 0,
            pending: None,
            complete: false,
        })
    }

    /// Build the next exact-owner GET; call `advance` on its response before another request.
    ///
    /// # Errors
    /// Enforces completion, ordering and total page/byte budgets before dispatch.
    pub fn next_request(&mut self) -> Result<LivyRequest<StatementSnapshot>, CursorError> {
        if self.complete {
            return Err(CursorError::AlreadyComplete);
        }
        if self.pending.is_some() {
            return Err(CursorError::OutOfOrder);
        }
        if self.requested >= self.max_pages || self.received_bytes >= self.max_total_bytes {
            return Err(CursorError::Limit);
        }
        let page = BytePage::new(Some(self.offset), Some(self.size))
            .map_err(|_| CursorError::InvalidInput)?;
        let request = self
            .statement
            .get(page)
            .map_err(|_| CursorError::InvalidRequest)?;
        self.pending = Some(request.native().clone());
        self.requested += 1;
        Ok(request)
    }

    /// Validate one response without modifying its raw fields or consuming its fragments.
    ///
    /// # Errors
    /// Rejects wrong owners/queries, accepted or unfinished/error output, missing,
    /// malformed, stalled or retrieval-limited cursors and cumulative byte excess.
    /// On error no cursor is advanced and no response is silently discarded.
    pub fn advance(
        &mut self,
        response: &LivyResponse<StatementSnapshot>,
    ) -> Result<PageProgress, CursorError> {
        let expected = self.pending.as_ref().ok_or(CursorError::OutOfOrder)?;
        if !Arc::ptr_eq(&self.statement.scope.binding, &response.binding)
            || self.statement.scope.acquisition != response.acquisition
            || expected != response.request()
        {
            return Err(CursorError::OwnerMismatch);
        }
        if response.status() != 200 {
            return Err(CursorError::NotSuccessful);
        }
        let model = response.model().ok_or(CursorError::InvalidMetadata)?;
        if model.observe().map_err(|_| CursorError::NotSuccessful)? != Observation::Succeeded {
            return Err(CursorError::NotSuccessful);
        }
        let bytes = self
            .received_bytes
            .checked_add(response.raw().len())
            .ok_or(CursorError::Limit)?;
        if bytes > self.max_total_bytes {
            return Err(CursorError::Limit);
        }
        let metadata = model
            .cursor_metadata()
            .ok_or(CursorError::InvalidMetadata)?;
        if !metadata.is_object() {
            return Err(CursorError::InvalidMetadata);
        }
        if metadata
            .get("from")
            .is_some_and(|start| start.as_u64() != Some(self.offset))
        {
            return Err(CursorError::InvalidMetadata);
        }
        let cursor = metadata
            .get("next")
            .and_then(Value::as_i64)
            .ok_or(CursorError::InvalidMetadata)?;
        let progress = match cursor {
            -1 => PageProgress::Complete,
            -2 => return Err(CursorError::RetrievalLimit),
            value if value < 0 => return Err(CursorError::InvalidMetadata),
            value if value as u64 <= self.offset => return Err(CursorError::NonAdvancing),
            value => PageProgress::More {
                next_offset: value as u64,
            },
        };
        match progress {
            PageProgress::Complete => self.complete = true,
            PageProgress::More { next_offset } => self.offset = next_offset,
        }
        self.received_bytes = bytes;
        self.pending = None;
        Ok(progress)
    }

    /// Current exact provider byte offset, never derived from output encoding.
    pub const fn offset(&self) -> u64 {
        self.offset
    }
    /// Number of requests built against the finite page budget.
    pub const fn requested_pages(&self) -> usize {
        self.requested
    }
    /// Bytes in successfully observed raw responses.
    pub const fn received_bytes(&self) -> usize {
        self.received_bytes
    }
    /// Remaining cumulative raw-response budget to pass to the existing executor.
    pub fn remaining_bytes(&self) -> usize {
        self.max_total_bytes - self.received_bytes
    }
}

impl fmt::Debug for StatementPages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatementPages")
            .field("offset", &self.offset)
            .field("requested", &self.requested)
            .field("complete", &self.complete)
            .field("owner", &"<scoped>")
            .finish()
    }
}
