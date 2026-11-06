// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Deterministic native Ontology authoring over an injected private workload executor.
//!
//! No HTTP client, credentials, host routing, runtime, retry or polling is installed.

pub mod bindings;
pub mod capabilities;
pub mod client;
pub mod models;
pub mod rdf;
pub mod scripts;
pub mod workload;
