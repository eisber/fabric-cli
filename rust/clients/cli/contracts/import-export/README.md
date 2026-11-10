<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared import/export interfaces

`fabric-cli-import-export-contracts` owns the existing `Transfer*` command values,
metadata/UI/file interfaces, results and original failure evidence shared by
definition import and export. The new `DefinitionCommandContext` borrows the
original host inputs and applies their shared bounded target/cancellation
preconditions. `src/local.rs` retains the confined `LocalTransferFiles` adapter;
`src/files.rs` adapts its injected file policy to existing definition loaders.

This is an independently registered package, not a module of the general
`fabric-cli-command-contracts` package. Its deployment/SDK dependencies are
specific to these interfaces and do not become dependencies of general API/ACL
contracts. It depends on neither command owner nor any compatibility facade.
It contains no import/export service algorithm, credential discovery, runtime,
terminal UI, mutation replay or second metadata cache.

`TransferCommand` retains deployment-only fields and `TransferKind::Deploy`
because the original aggregate argument contract must remain compatible.
Focused command owners add their own schemas to the shared root flags and reuse
the original token decoder. The
[compatibility adapter](../../compatibility/import-export/README.md) reexports the
shared values and callbacks, while preserving its original nominal error type,
envelope method and output policy. Converting a raw workflow error moves all
evidence; it neither renders nor discards the original failure.
