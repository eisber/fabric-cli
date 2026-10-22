// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native deployment library boundary, intentionally without deployment APIs yet.
//!
//! Future modules will implement configuration compatibility, transforms,
//! dependency planning, publish/unpublish workflows, and operation reconciliation
//! using `fabric-sdk`. No Python bridge, subprocess, or runtime is provided.
//! Dependencies will be added only when exercised by a bounded implementation.
