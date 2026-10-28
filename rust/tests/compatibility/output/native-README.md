<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Extended JSON value contracts

`native-cases.json` defines lossless encoding inputs using decimal integer
strings, IEEE 754 bit patterns and explicit Unicode codepoints. The JSON fixture
carrier is not itself the runtime value: integer rounding, surrogate repair and
nonfinite-to-null conversion are forbidden.

Contracts cover wide integers, signed zero, NaN and infinity, exact string
codepoints, surrogate identity, duplicate-key ordering, nested containers,
truthiness and default/pretty layouts.

## Public APIs

`encode_python` and `encode_python_with_options` consume `PythonValue`.
`DecimalInteger` validates decimal grammar without f64 conversion.
`PythonString` retains validated codepoints, and `PythonDict` replaces duplicate
values in their original insertion position.

`try_to_json` rejects values outside the ordinary JSON domain; `try_from_json`
preserves known finite kinds. Unicode output rejects surrogate-containing
strings that cannot be represented as valid UTF-8 rather than inserting
replacement characters.

## Limits

Tests check exact and one-past limits for integer digits, string codepoints,
dictionary inputs, depth, nodes and encoded bytes. Traversal limits do not
retroactively bound a caller's already-allocated tree or clone/debug operations.
Existing finite-value APIs retain their separate contracts.

From `rust/`, with caller-owned observation files:

```text
cargo run --offline --locked -q -p fabric-core-json --example native_json
python -B tests\compatibility\output\native_reference.py --encoding-only --observed <artifact>
python -B -m unittest discover -s tests\compatibility\output -p test_native_reference.py
```

The example emits observations only. The reference owns immutable expectations;
negative controls reject missing cases, altered numeric spelling, reordered
keys, surrogate loss and layout substitution. No live service or credential
access occurs.
