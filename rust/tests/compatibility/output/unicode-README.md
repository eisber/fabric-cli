<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit Unicode definition JSON

Requested consumer API: `fabric_core_json::encode_unicode(&Value, Format) -> String`,
with `Format::Pretty(Indent::Four)` for definitions. Both it and the additive
`encode_with_options` delegate to the one existing string/number/container engine.
Status: implemented and locally validated under the reviewed gate v2, with all
18 real-source byte comparisons, four Python guard tests, five core consumer
tests and two core doctests passing. Seven retained CLI consumer tests and all
42 original envelope comparisons also pass. Strict core all-target Clippy,
Python Black/mypy and workspace format checks pass. The completed gate receipt
records mutex release at 2026-10-26T12:12:36.569Z. This evidence is scoped to the
Unicode formatting option, not the separate native-value drafts.

Source pin: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. The caller use
case is `utils/fab_storage.py`'s `json.dump(..., indent=4, ensure_ascii=False)`;
the pure value serializer performs no file writes or command orchestration.
`unicode_reference.py` executes the exact whole-blob-pinned storage `json.dump`
call expression against private `StringIO` (no filesystem writes), actual
Python `json.dumps`, and the existing
whole-blob-verified `utils/fab_util.py::dumps` with these explicit keyword
arguments. It compares full strict UTF-8 bytes, not just parsed JSON.

`unicode-json-cases.json` adds 18 independent, hand-authored synthetic cases
covering BMP/astral values and keys, DEL, Unicode separators/nonprintables,
JSON controls and quotes/backslashes/slashes, nested container layout/order,
empty/falsey values, finite integer/f64 edge spellings, and all existing indent
modes including the default no-indent mode. Each observation additionally emits
the legacy ASCII path and compares it to Python's unchanged default behavior.
No prior golden or source expectation is regenerated or relaxed.

`test_unicode_reference.py` rejects ASCII-escaped replacements (even with
identical semantic JSON), changed legacy output, key sorting, missing cases,
CRLF normalization and added newlines. Source-only native-value checks keep
NaN, arbitrary integers and lone surrogates explicit: the false option does not
claim to make them representable by `serde_json::Value`. In particular, Python's
lone-surrogate result fails strict UTF-8 encoding rather than authorizing
replacement characters.

From `rust/`, under the named shared-host gate:

```text
cargo test --offline --locked -p fabric-core-json
cargo run --offline --locked -q -p fabric-core-json --example unicode_json
python -B tests\compatibility\output\unicode_reference.py --observed <session-artifact>
python -B -m unittest discover -s tests\compatibility\output -p test_unicode_reference.py
```

The Rust example emits observations only. The independent source oracle owns
expectations. Tests are offline and use no ambient terminal, clock, auth,
network, product module import or global module patch.
