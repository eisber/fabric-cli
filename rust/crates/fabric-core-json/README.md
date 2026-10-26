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

For definition metadata/export bytes matching
`json.dumps(data, ensure_ascii=False, indent=4)`, use the additive public API
`encode_unicode(&Value, Format) -> String`:

```rust
use fabric_core_json::{Format, Indent, encode_unicode};
use serde_json::json;

let encoded = encode_unicode(&json!({"displayName": "caf\u{e9}"}), Format::Pretty(Indent::Four));
assert_eq!(encoded, "{\n    \"displayName\": \"caf\u{e9}\"\n}");
```

`encode_with_options` also exposes the same engine's explicit `Options` policy.
`Options::default()` retains Python's default `ensure_ascii=true` and no-indent
layout. Existing `encode`, `json_dumps`, all pretty layouts and CLI wrappers
still escape non-ASCII exactly as before. The explicit false mode retains valid
Unicode scalars (including astral characters, DEL, Unicode separators and
noncharacters) as UTF-8, while escaping JSON quotes/backslashes and control
characters below U+0020. Slash remains unescaped. Keys and values follow the
same policy and insertion order; no trailing newline or file I/O is added.

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

The option does **not** widen the value domain. Python can emit nonfinite
numbers or arbitrary-width integers and can return a surrogate-containing string
that fails strict UTF-8 encoding; those native-value branches are separately
tracked, not silently normalized, replaced or emitted as null here. New
`unicode-json-cases.json` expectations and `unicode_reference.py` compare actual
UTF-8 bytes to real Python and the immutable source `fab_util.dumps` helper.
`tests/unicode.rs` and the standalone `unicode_json` consumer also check the
legacy ASCII output side by side.
