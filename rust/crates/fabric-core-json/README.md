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

Inputs to the legacy `Value` APIs are already JSON-domain values (i64/u64 and finite f64). Those APIs do
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

## Additive native Python values

`PythonValue` adds caller-owned None/bool/validated decimal integer/f64/string/
list/ordered string-key dict values without replacing any finite `Value` API.
`DecimalInteger` retains canonical decimal magnitude without numeric casts;
`PythonString` retains codepoints including lone/adjacent surrogates;
`PythonDict` replaces duplicate values in their first insertion position.

```rust
use fabric_core_json::{DecimalInteger, Format, PythonString, PythonValue, encode_python};

let value = PythonValue::List(vec![
    PythonValue::Integer(DecimalInteger::parse("18446744073709551616")?),
    PythonValue::Float(f64::NAN),
    PythonValue::String(PythonString::from_codepoints([0xd800])?),
]);
assert_eq!(encode_python(&value, Format::Default)?,
    "[18446744073709551616, NaN, \"\\ud800\"]");
assert!(value.try_to_json().is_err());
# Ok::<(), fabric_core_json::PythonValueError>(())
```

`encode_python` uses Python ASCII/default-allow-NaN encoding;
`encode_python_with_options` can select existing layout/Unicode policy but
explicitly fails for surrogate-containing UTF-8 output. Both reuse the same
string/number/layout engine as `encode`, `json_dumps` and `encode_unicode`.
`try_to_json`/`try_from_json` are typed finite conversion boundaries; unsupported
wide integers, nonfinite floats and surrogate keys/values never become rounded
numbers, null or replacement strings. Truthiness is explicit via `is_truthy`.

This encoding-only increment has no arbitrary-body JSON parser, service
deserializer, native CLI output envelope, repr/str renderer, Unicode database,
auth/runtime or new dependency. Python bytes/custom objects/cycles/non-string
dict keys are unsupported. Explicit safety bounds are 4300 decimal digits,
1,048,576 codepoints per string, 100,000 dictionary inputs/tree values, depth128,
and 16MiB encoded output. These are documented safety exceptions, not full
Python process settings parity. See the [native source cases and exact boundary
classification](../../tests/compatibility/output/native-README.md).

The native-value slice's initial 35-case source comparisons pass; this immutable
**source candidate has final validation pending on isolated hosted CI**.
Subsequently added exact-limit tests could not execute locally because shared-host
guards refused before validation started. The ledger preserves both passing and
failed receipts and specifies the required final commands; a candidate commit
does not turn unexecuted tests green. Do not interpret existing released
Unicode/finite JSON checks as proof that a native service-body parser or CLI
adapter is available.
