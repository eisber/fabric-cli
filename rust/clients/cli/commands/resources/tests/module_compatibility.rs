// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Role-named entry points retain the exact original public command types.

#![deny(unnameable_test_items)]

use fabric_cli_mutations::{frontdoor, host, parser};

#[test]
fn legacy_module_is_a_same_type_reexport() {
    let _: fn(parser::ResourceCommand) -> frontdoor::ResourceCommand = std::convert::identity;
    let _: fn(parser::ResourceArgs) -> frontdoor::ResourceArgs = std::convert::identity;
    let _: fn(host::ResourceConfig) -> frontdoor::ResourceConfig = std::convert::identity;
}
