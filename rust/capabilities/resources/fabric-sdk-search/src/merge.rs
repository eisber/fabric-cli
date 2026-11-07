// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    Candidate, CandidateIdentity, CatalogCursor, SearchData, SearchError, SearchMode,
    SearchReceipt, SearchRequest, SearchStatus, Source, SourceEvidence, SourceObservation,
    SourceStatus,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Pure deterministic federation over already acquired, scope-verified observations.
///
/// Both selected source slots are mandatory; nonselected slots must be absent.
/// Identity overlap retains all provenance, with Catalog placement taking
/// precedence. Alternation starts with Catalog, retaining each source's original
/// order after identity deduplication. No score comparison or name join occurs.
///
/// # Errors
/// Rejects source/scope mismatches, ownership/type conflicts, both-source failure
/// and finite response/receipt budget violations. It never performs I/O.
pub fn federate(
    request: &SearchRequest,
    catalog: Option<SourceObservation>,
    relevance: Option<SourceObservation>,
) -> Result<SearchReceipt, SearchError> {
    let mode = request.mode();
    let selected = match mode {
        SearchMode::Catalog => [true, false],
        SearchMode::Relevance => [false, true],
        SearchMode::Hybrid => [true, true],
    };
    let mut observations = [catalog, relevance];
    let sources = [Source::Catalog, Source::Relevance];
    let mut reported_bytes = 0usize;
    let mut serialized_bytes = 0usize;
    let mut requests = 0usize;
    for (index, observation) in observations.iter_mut().enumerate() {
        if observation.is_some() != selected[index] {
            return Err(if selected[index] {
                SearchError::MissingProvider {
                    provider: sources[index],
                }
            } else {
                SearchError::InvalidObservation
            });
        }
        let Some(observation) = observation else {
            continue;
        };
        if observation.evidence.source != sources[index] {
            return Err(SearchError::InvalidObservation);
        }
        reported_bytes = reported_bytes
            .checked_add(observation.evidence.response_bytes.unwrap_or(0))
            .ok_or(SearchError::BudgetExceeded)?;
        serialized_bytes = serialized_bytes
            .checked_add(crate::model::check_bytes(
                observation,
                request.limits().max_bytes(),
            )?)
            .ok_or(SearchError::BudgetExceeded)?;
        requests += observation.evidence.requests;
        if reported_bytes > request.limits().max_bytes()
            || observation.evidence.response_bytes.is_some_and(|bytes| {
                bytes
                    > request.limits().max_bytes() / if mode == SearchMode::Hybrid { 2 } else { 1 }
            })
            || serialized_bytes > request.limits().max_bytes()
            || requests > request.limits().max_requests()
        {
            return Err(SearchError::BudgetExceeded);
        }
        let too_many = match (sources[index], request) {
            (Source::Catalog, SearchRequest::Initial(initial)) => {
                observation.candidates.len() > initial.limits().page_size()
            }
            (Source::Relevance, _) => observation.candidates.len() > 50,
            _ => false,
        };
        if too_many
            || observation.candidates.iter().any(|candidate| {
                candidate.identity().tenant_id() != request.scope().tenant_id()
                    || request
                        .scope()
                        .workspace_id()
                        .is_some_and(|workspace| candidate.workspace_id() != Some(workspace))
                    || match request {
                        SearchRequest::Initial(initial) => {
                            let filters = initial.filters();
                            (!filters.include().is_empty()
                                && !filters
                                    .include()
                                    .iter()
                                    .any(|value| value == candidate.artifact_type()))
                                || filters
                                    .exclude()
                                    .iter()
                                    .any(|value| value == candidate.artifact_type())
                        }
                        SearchRequest::CatalogResume(_) => false,
                    }
            })
        {
            *observation = observation.clone().reject();
        }
    }

    let mut evidence = Vec::new();
    let mut union: BTreeMap<CandidateIdentity, Candidate> = BTreeMap::new();
    let mut order: [VecDeque<CandidateIdentity>; 2] = Default::default();
    let mut usable = 0usize;
    let mut partial = false;
    for (index, observation) in observations.into_iter().enumerate() {
        let Some(observation) = observation else {
            evidence.push(SourceEvidence::not_selected(sources[index]));
            continue;
        };
        if observation.evidence.status != SourceStatus::Failed {
            usable += 1;
        }
        partial |= matches!(
            observation.evidence.status,
            SourceStatus::Failed | SourceStatus::Limited
        );
        evidence.push(observation.evidence);
        let mut seen = BTreeSet::new();
        for candidate in observation.candidates {
            let identity = candidate.identity().clone();
            if seen.insert(identity.clone()) {
                // Catalog owns placement of overlaps; RE evidence is still merged.
                if index == 0 || !union.contains_key(&identity) {
                    order[index].push_back(identity.clone());
                }
            }
            match union.get_mut(&identity) {
                Some(existing) => existing.merge(candidate)?,
                None => {
                    union.insert(identity, candidate);
                }
            }
        }
    }
    if usable == 0 {
        return Err(SearchError::AllSourcesFailed { sources: evidence });
    }
    let dropped = union.len().saturating_sub(request.limits().max_results());
    partial |= dropped != 0;
    let mut hits = Vec::new();
    while hits.len() < request.limits().max_results()
        && (!order[0].is_empty() || !order[1].is_empty())
    {
        for queue in &mut order {
            if hits.len() == request.limits().max_results() {
                break;
            }
            if let Some(identity) = queue.pop_front() {
                hits.push(
                    union
                        .remove(&identity)
                        .ok_or(SearchError::InvalidObservation)?,
                );
            }
        }
    }
    let continuation = if mode == SearchMode::Catalog && dropped == 0 {
        match &evidence[0].coverage {
            crate::Coverage::CatalogPage {
                continuation: Some(token),
            } => Some(CatalogCursor::from_native(
                request.scope().clone(),
                token.clone(),
            )?),
            _ => None,
        }
    } else {
        None
    };
    let receipt = SearchReceipt {
        schema_version: "0.1",
        mode,
        scope: request.scope().clone(),
        status: if partial {
            SearchStatus::Partial
        } else {
            SearchStatus::Succeeded
        },
        data: SearchData { hits },
        sources: evidence,
        continuation,
        dropped_candidates: dropped,
    };
    crate::model::check_bytes(&receipt, request.limits().max_bytes())?;
    Ok(receipt)
}
