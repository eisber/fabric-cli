// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Notebook service APIs with caller-owned credentials, execution and cancellation.
//!
//! This package owns Notebook definitions, requests and bounded service sessions.
//! Repository preparation and deployment orchestration belong to `fabric-deploy`.

mod client;
mod models;
pub use client::NotebookSession;
pub use models::{
    DefinitionPart, NotebookDefinition, NotebookError, NotebookErrorKind, NotebookExecutionError,
    NotebookExecutor, NotebookItem, NotebookLimits, NotebookRequest, NotebookResponse,
};
