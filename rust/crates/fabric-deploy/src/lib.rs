// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native single-Notebook deployment through injected Fabric SDK execution.
//!
//! Load and prepare local files synchronously before starting asynchronous service
//! work. No Python bridge, credential fallback, CLI state, logger or runtime is
//! installed. Broad YAML/JSONPath/regex deployment compatibility remains separate.

#![doc = include_str!("../README.md")]

pub mod config;
pub mod notebook;
pub mod transforms;
