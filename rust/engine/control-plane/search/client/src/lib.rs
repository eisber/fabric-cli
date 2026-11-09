// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, caller-injected search and pure source-aware federation.
//!
//! This library owns no runtime, credentials, HTTP transport, approval policy or
//! CLI state. Concrete providers map caller-bound Catalog/Relevance SDK clients;
//! they do not install a native origin, authentication or HTTP execution host.
//! Request validation, SDK JSON input and public schemas share one contract module.

#![cfg_attr(test, deny(unnameable_test_items))]

mod bridge;
mod contract;
mod executor;
mod merge;
mod model;
mod providers;

pub use bridge::{
    SEARCH_OPERATION_ID, SearchBridgeError, SearchSourceDetails, execute_approved_search,
    prepare_search, search_descriptor,
};
pub use contract::{
    CatalogCursor, CatalogResume, Guid, InitialSearch, SearchLimits, SearchMode, SearchRequest,
    SearchSchemas, SearchScope, TypeFilters, schemas,
};
pub use executor::{
    ProviderCapabilities, ProviderRequest, SearchFuture, SearchOperation, SearchProvider,
};
pub use merge::federate;
pub use model::{
    Candidate, CandidateIdentity, CandidateMetadata, Coverage, LimitReason, Provenance,
    ProviderError, ProviderErrorCode, SearchData, SearchError, SearchReceipt, SearchStatus, Source,
    SourceEvidence, SourceObservation, SourceStatus, canonical_type,
};
pub use providers::{CatalogProvider, RelevanceProvider};
