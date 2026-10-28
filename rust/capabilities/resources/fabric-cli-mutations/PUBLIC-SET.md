<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared pure SET helper

`fabric_cli_mutations::update_fabric_element(Value, &str, &str)` is the existing
metadata mutation implementation, re-exported for same-layer CLI consumers.
It parses the source input, replaces the selected field/index and returns only
the raw query's changed top-level property. No implementation is copied or changed.
The source `values[0]` top-level projection oddity remains an empty object.

This is not a generic resource validator. Callers must first apply their source
command's allowlist/blocklist/simple-path policy, fetch the actual source metadata
with the correct audience, and enforce their existing confirmation/force behavior.
The helper performs no request, selection, credential discovery, prompt, cache
update or output. Explicit errors retain the existing finite JSON boundaries.

`tests/public_set.rs` and `examples/public_set.rs` are external public consumers.
`tests/public_set_reference.py` executes unchanged pinned Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` SET/JMESPath helper bodies with the pinned
development-only query evaluator; no Python is required by shipped Rust.
SDK crates must never depend on this CLI-layer helper.
