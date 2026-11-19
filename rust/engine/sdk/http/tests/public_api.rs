// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The compatible executor retains inherent associated-function imports.

#![deny(unnameable_test_items)]

use fabric_sdk_http::HttpExecutor;

#[test]
fn original_inherent_execution_entrypoints_remain_addressable() {
    let _: fn(&HttpExecutor) -> &fabric_core::EndpointBinding = HttpExecutor::endpoint;
    let _ = HttpExecutor::execute_fabric;
    let _ = HttpExecutor::execute_bound;
    let _ = HttpExecutor::execute_bound_with_progress;
    let _ = HttpExecutor::wait_for;
    let _ = HttpExecutor::with_endpoint;
    let _ = HttpExecutor::with_timeout;
    let _ = HttpExecutor::with_trace;
}
