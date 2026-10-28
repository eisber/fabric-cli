<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Ordered JSON encoding

`fabric-core-json` supplies one ordered JSON encoder for request bodies,
deployment definitions and CLI output. It is a pure core-layer library without
runtime, filesystem, logging, identity or service dependencies.

## JSON values

`json_dumps(&Value)` emits comma-space and colon-space separators, ASCII escapes,
insertion order and no final newline. `encode` supports the same scalar policy
with `Format::Pretty` and indentation of zero, two or four spaces.

`encode_unicode` retains valid Unicode scalars as UTF-8 while escaping JSON
quotes, backslashes and control characters:

```rust
use fabric_core_json::{Format, Indent, encode_unicode};
use serde_json::json;

let encoded = encode_unicode(&json!({"displayName": "caf\u{e9}"}), Format::Pretty(Indent::Four));
assert_eq!(encoded, "{\n    \"displayName\": \"caf\u{e9}\"\n}");
```

`encode_with_options` selects formatting and escaping explicitly. Existing
`Value` APIs accept JSON-domain integers and finite floats; they cannot recover
information lost before conversion. Ordered maps and finite-float parsing use
the centralized `serde_json` features.

## Extended value representation

`PythonValue` is the public extended-value model for validated decimal integers,
floating-point values, codepoint strings, lists and ordered string-key maps.
`DecimalInteger` preserves magnitude without numeric casts. `PythonString`
can retain surrogate codepoints; `PythonDict` replaces duplicate values in
their first insertion position.

`encode_python` makes nonstandard numeric and string representation explicit.
`try_to_json` rejects values outside the ordinary JSON domain rather than
rounding integers or converting nonfinite values to null. Callers choose this
model deliberately; it does not widen existing `Value` APIs.

Encoding is bounded where exposed by the API, preserves key/value order and
adds no file writes or newline conversion. CLI envelope wrappers delegate to
this implementation rather than maintaining another serializer.
