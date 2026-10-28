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
inputs, not global mutable state. Root owns its module registration and the
native/`FamilySession` dispatch injection; this lane does not edit native
runtime or family dispatch code and adds no required fields to those hosts.
The existing explicit-map `DeployInputs` API and its empty default remain valid.

## Parameter input is not authentication

The existing `enable_environment_variable_replacement` feature flag controls
whether deployment parameter expansion consumes this map. The snapshot does
not enable that flag or interpret any name, including names resembling tokens,
accounts or providers. It is never passed to identity selection, token
providers, credential discovery or authentication fallbacks.

Environment values may be sensitive. The helper does not log, persist or echo
them; error diagnostics contain no names or values. `DeployInputs::Debug`
redaction is a separately coordinated change confined to that struct's Debug
region. Native integration must retain the redacted projection.

## Offline validation boundary

`tests/native_deploy_inputs.rs` uses synthetic iterators only. It never invokes
the native snapshot, reads the real environment, opens profiles, discovers
credentials or sends requests. Invalid Unicode is constructed in memory with
platform-native `OsString` constructors. The test crate denies accidental
unnamable test items.

Cargo compilation/tests remain pending the root's authorized validation slot;
formatting is not compilation or evidence of complete native deployment parity.
