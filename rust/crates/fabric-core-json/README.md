<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared Python-style JSON encoder

`fabric-core-json` is a pure **core-layer** leaf for raw SDK request bodies,
deployment definitions and CLI serialization. It has no SDK, CLI, identity,
runtime, clock, environment, logging or I/O dependency.

`json_dumps(&serde_json::Value) -> String` emits Python-default no-indent JSON:
comma-space, colon-space, ASCII escaping, insertion order and no newline.
`encode(&Value, Format::Pretty(Indent::{Zero, Two, Four}))` uses the same
string/number implementation for the existing CLI pretty envelope layouts.
`Indent::Zero` is not the default: it emits container newlines.

This extracts, rather than copies, the source-pinned CLI encoder.
`fabric-cli-output::json_dumps` and its envelope APIs remain compatibility
wrappers, with the same types and results. There is exactly one escaping/numeric
implementation, not competing SDK/CLI serializers.

Source: Python Fabric CLI `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`utils/fab_util.py::dumps`, plus the output constructors' pretty JSON. Public
consumer tests use the same independently authored literal bytes; standalone
core and CLI examples emit observations compared with real Python `json.dumps`
and the pinned helper. No expected fixture is regenerated.

Inputs are already JSON-domain values (i64/u64 and finite f64). The API does
not convert arbitrary `Serialize` inputs, infer prior lossy conversions, emit
nonfinite Python literals, preserve arbitrary-width Python integers, accept
unpaired surrogates, or redact Python runtime bytes. Existing numeric exceptions
and measured-not-exhaustive f64 byte claims remain unchanged. Callers validate
unsupported raw inputs before converting to `Value`. JSON ordered maps and
finite-float parsing require the existing centralized serde_json
`preserve_order`/`float_roundtrip` features, not new dependency versions.
