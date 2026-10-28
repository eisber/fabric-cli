<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native deployment parameter environment

Base: root `c3bdd6ce32da91426bb39d03eae22200fa350189`, including the adopted
full deployment consumer. This additive binary boundary supplies the existing
`DeployInputs::environment` map; it does not change parameter expansion,
deployment execution, configuration flags or SDK authentication.

## Private binary interface

`native_deploy_inputs.rs` provides:

```text
select_environment(IntoIterator<Item = (OsString, OsString)>)
    -> Result<DeployInputs, EnvironmentInputError>
snapshot_environment()
    -> Result<DeployInputs, EnvironmentInputError>
```

The selector is pure. Names and values are preserved as supplied, including
empty values and Unicode. Duplicate names use the last value. It does not
case-fold names, trim values, scan configuration or select a credential.
Non-Unicode names and values reject the complete selection with distinct typed
errors that contain no offending input. No entry is silently discarded.

The snapshot function is the sole process-environment reader and is invoked
only explicitly by the native binary. It captures command-owned parameter
inputs, not global mutable state. The native deployment branch registers this
module, obtains the checked snapshot and passes it through the additive
`FamilySession::dispatch_with_deploy_inputs` entry point. Invalid Unicode becomes
an explicit input failure before deployment dispatch. No required fields are
added to the family host.
The existing explicit-map `DeployInputs` API and its empty default remain valid.
Root's additive `FamilySession::dispatch_with_deploy_inputs` seam accepts this
map by reference. Existing `dispatch` keeps its empty-map default; only the
native deploy caller should invoke the explicit snapshot and new dispatch seam.
Other families and authentication inputs are unchanged.

## Parameter input is not authentication

The existing `enable_environment_variable_replacement` feature flag controls
whether deployment parameter expansion consumes this map. The snapshot does
not enable that flag or interpret any name, including names resembling tokens,
accounts or providers. It is never passed to identity selection, token
providers, credential discovery or authentication fallbacks.

Environment values may be sensitive. The helper does not log, persist or echo
them; error diagnostics contain no names or values. `DeployInputs::Debug`
emits a redacted placeholder and entry count only, including alternate/pretty
formatting. Neither names nor values are formatted. That change is confined to
the struct's derive/Debug region; its public fields, clone/default behavior and
deployment execution are unchanged. Native integration must retain this
redacted projection.

## Offline validation boundary

`tests/native_deploy_inputs.rs` uses synthetic iterators only. It never invokes
the native snapshot, reads the real environment, opens profiles, discovers
credentials or sends requests. Invalid Unicode is constructed in memory with
platform-native `OsString` constructors. The test crate denies accidental
unnamable test items. `tests/deploy_inputs_debug.rs` separately checks normal
and pretty redaction with synthetic values and preserves default/clone/explicit
map behavior. No protected oracle or existing expectation is changed.

Cargo compilation/tests remain pending the root's authorized validation slot;
formatting is not compilation or evidence of complete native deployment parity.

The smallest requested root validation command, from `rust/`, is:

```text
cargo test --locked -p fabric-cli --test native_deploy_inputs --test deploy_inputs_debug
```

The full root consumer additionally injects synthetic environment input and
checks expansion before YAML decoding and publication. Actual binary mounting,
that integration case and the helper/debug cases still require executed native
validation. The process snapshot is never invoked by these offline tests.
