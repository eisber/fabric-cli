// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{cell::RefCell, error::Error, fmt, sync::Arc};

pub use fabric_name_search::{SearchError, SearchOptions};

use crate::{
    ContextError, Element, ErrorKind, HierarchyLookup, Lookup, LookupKind, LookupRequest,
    OneLakeProperties, VirtualLookupRequest,
};

/// A names-only, lazily visited view of an already supplied immutable snapshot.
///
/// Names exclude their artifact suffix. The lookup implementation must restrict
/// them to the request's exact scope and suffix/type before exposing this view.
pub type CandidateNames<'a> = Box<dyn Iterator<Item = &'a str> + 'a>;

/// One advisory name, never a resolved target or an authorization to execute.
#[derive(Clone, Debug, PartialEq)]
pub struct NameHint {
    /// Suggested name; resolver-produced names include the canonical suffix.
    ///
    /// This is unescaped snapshot data. Use [`Self::escaped_name`] or `Display`
    /// for terminal text; never use this field as an alternate execution target.
    pub name: String,
    /// Similarity score from the shared search engine.
    pub score: f64,
}

impl NameHint {
    /// Escape controls, quotes, backslashes and non-ASCII characters for text.
    pub fn escaped_name(&self) -> String {
        self.name.chars().flat_map(char::escape_default).collect()
    }
}

impl fmt::Display for NameHint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "\"{}\"", self.escaped_name())
    }
}

/// Why an eligible exact miss could not supply advisory names.
#[derive(Debug)]
pub enum NameHintsUnavailable {
    /// The caller did not opt its lookup into exposing a names-only snapshot.
    SnapshotNotProvided,
    /// Reading the caller's snapshot failed; no names are exposed.
    SnapshotFailure,
    /// The shared engine rejected options or exceeded a fixed search bound.
    Search(SearchError),
}

impl fmt::Display for NameHintsUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SnapshotNotProvided => {
                formatter.write_str("Name hints unavailable: no names-only snapshot supplied.")
            }
            Self::SnapshotFailure => {
                formatter.write_str("Name hints unavailable: names-only snapshot lookup failed.")
            }
            Self::Search(_) => {
                formatter.write_str("Name hints unavailable: search options or limits rejected.")
            }
        }
    }
}

/// Structured advisory data kept separate from the original resolution failure.
#[derive(Debug)]
pub enum NameHints {
    /// Not an eligible Fabric exact-name miss (including suppressed lookups).
    NotApplicable,
    /// An exact miss occurred, but hints could not safely be computed.
    Unavailable(NameHintsUnavailable),
    /// Ranked names from the exact failed scope/type; may be empty.
    Suggestions(Vec<NameHint>),
}

impl fmt::Display for NameHints {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApplicable => Ok(()),
            Self::Unavailable(reason) => fmt::Display::fmt(reason, formatter),
            Self::Suggestions(names) if names.is_empty() => Ok(()),
            Self::Suggestions(names) => {
                formatter.write_str("Similar names (not selected): ")?;
                for (index, name) in names.iter().enumerate() {
                    if index != 0 {
                        formatter.write_str(", ")?;
                    }
                    fmt::Display::fmt(name, formatter)?;
                }
                Ok(())
            }
        }
    }
}

/// Additive failure envelope; the original failure and its display stay intact.
///
/// Display renders only `failure`. Render `hints` separately when desired.
#[derive(Debug)]
pub struct NameHintFailure<E> {
    /// Unmodified legacy failure, including notices for extended resolution.
    pub failure: E,
    /// Advisory names or an explicit unavailable diagnostic, never a target.
    pub hints: NameHints,
}

impl<E: fmt::Display> fmt::Display for NameHintFailure<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.failure, formatter)
    }
}

impl<E: Error + 'static> Error for NameHintFailure<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.failure)
    }
}

struct Miss {
    kind: LookupKind,
    parent: Arc<Element>,
    name: String,
}

/// Records only an exact lookup miss. Candidate access is deferred until the
/// complete resolution fails, so local fallback and other failures cannot hint.
pub(crate) struct HintLookup<'a, L: Lookup + ?Sized> {
    lookup: &'a L,
    miss: RefCell<Option<Miss>>,
    enabled: bool,
}

impl<'a, L: Lookup + ?Sized> HintLookup<'a, L> {
    pub(crate) fn new(lookup: &'a L, enabled: bool) -> Self {
        Self {
            lookup,
            miss: RefCell::new(None),
            enabled,
        }
    }

    pub(crate) fn hints(&self, error: &ContextError, options: SearchOptions) -> NameHints {
        if !self.enabled || !is_not_found(error) {
            return NameHints::NotApplicable;
        }
        let miss = self.miss.borrow();
        let Some(miss) = miss.as_ref() else {
            return NameHints::NotApplicable;
        };
        let Some((query, suffix)) = miss.name.rsplit_once('.') else {
            return NameHints::NotApplicable;
        };
        let candidates = match self.lookup.candidate_names(LookupRequest {
            kind: miss.kind,
            parent: &miss.parent,
            name: &miss.name,
        }) {
            Ok(Some(names)) => names,
            Ok(None) => {
                return NameHints::Unavailable(NameHintsUnavailable::SnapshotNotProvided);
            }
            Err(_) => return NameHints::Unavailable(NameHintsUnavailable::SnapshotFailure),
        };
        match fabric_name_search::suggest_names(query, candidates, options) {
            Ok(names) => NameHints::Suggestions(
                names
                    .into_iter()
                    .map(|name| NameHint {
                        name: format!("{}.{suffix}", name.name),
                        score: name.score,
                    })
                    .collect(),
            ),
            Err(error) => NameHints::Unavailable(NameHintsUnavailable::Search(error)),
        }
    }
}

impl<L: Lookup + ?Sized> Lookup for HintLookup<'_, L> {
    fn lookup(&self, request: LookupRequest<'_>) -> Result<Option<String>, ContextError> {
        let result = self.lookup.lookup(request);
        let absent = match &result {
            Ok(id) => id.as_deref().is_none_or(str::is_empty),
            Err(error) => is_not_found(error),
        };
        if self.enabled && absent {
            *self.miss.borrow_mut() = Some(Miss {
                kind: request.kind,
                parent: Arc::clone(request.parent),
                name: request.name.to_owned(),
            });
        }
        result
    }
}

impl<L: HierarchyLookup + ?Sized> HierarchyLookup for HintLookup<'_, L> {
    fn virtual_id(
        &self,
        request: VirtualLookupRequest<'_>,
    ) -> Result<Option<String>, ContextError> {
        self.lookup.virtual_id(request)
    }

    fn properties(&self, path: &str) -> Result<Option<OneLakeProperties>, ContextError> {
        self.lookup.properties(path)
    }
}

fn is_not_found(error: &ContextError) -> bool {
    error.code == "NotFound" && matches!(error.kind, ErrorKind::Python | ErrorKind::Lookup)
}
