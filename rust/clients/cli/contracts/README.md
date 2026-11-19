<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared CLI command contracts

`fabric-cli-command-contracts` owns the existing resource-command value, error,
confirmation, presentation and HTTP-adapter contracts. The capability adapters
under `commands/access`, `commands/api` and `commands/catalog` share these exact
types. `fabric-cli-resources` reexports them for source compatibility.

The extraction is behavior-preserving against v2 checkpoint
`61c8221914152e231e5e64f7bd38418f66264999`; the focused CLI contract layout at
v1 commit `f8cb97e92eab669785954335e0e7276d30a3377e` is prior art, not an
imported product baseline. Shared parser helpers retain token joining,
present-empty values and error precedence. Capability grammars and execution
stay outside this package.

This is a CLI-layer package, not an SDK dependency or a replacement for
`fabric-cli-output`. It selects no credential provider, runtime, configuration,
terminal, transport or consent. Its existing `ResourceHttpExecutor` only adapts
an explicitly supplied HTTP executor.
