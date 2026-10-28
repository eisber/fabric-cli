<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Lossless Python value follow-on

**Source candidate; final validation pending.** The integration owner authorized
an immutable candidate for isolated hosted validation after local final runs
were refused before starting. The first implementation's passed checks below
are real, but they do not certify the subsequently added exact-boundary tests.
No local guard is bypassed and no test/limit/oracle is waived by this candidate.

Scope: additive **encoding-only** `fabric-core-json` slice approved by the
integration owner in task message5878. Existing finite JSON APIs and committed
expectations remain unchanged. This does not implement a service/body parser,
new CLI envelope, jobs routing, Python str/repr renderer or Unicode property
database. The original source-only repr/str observations remain independent
evidence for a later CLI adapter, not proof of native repr support here.

Python source pin remains `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
The existing blob-verified real `fab_util.dumps` helper and real Python
`str`/`repr`/`bool` are the oracle. Cases specify integer decimal strings,
IEEE754 f64 bit patterns and Python Unicode codepoints before constructing actual
Python values. A JSON fixture carrier is **not** the runtime value itself:
no finite-f64 coercion, surrogate repair or arbitrary-int rounding occurs.

`native-cases.json` contains 35 hand-authored observations of null/bool, wide
integers, signed zero, signed/payload NaN, infinities, shortest-decimal tie,
subnormal, quote/control/Unicode printability, lone surrogates, a surrogate pair
distinct from one astral scalar, ordered dictionaries and nested values.
Additional cases pin integer sign/leading-zero canonicalization, duplicate keys'
last-value/first-position behavior, and distinct surrogate-pair/scalar keys
whose ASCII JSON spellings happen to be identical.
The oracle also independently obtains all three existing pretty layouts.
String observations use exact codepoints, permitting a real lone surrogate
without silently replacing it with U+FFFD or inventing UTF-8 bytes.

`test_native_reference.py` rejects mutations of arbitrary integers, nonfinite
JSON/repr spelling, truthiness, surrogate identity, default versus pretty
separators, and missing-case observations. Unsupported exotic Python runtime
objects remain outside this candidate native value tree.

The implementation shares the existing encoder through borrowed native/JSON
views, one recursive container/layout function, and one string/number formatter.
It does not add a second
serializer, SDK-to-CLI edge, unchecked raw JSON token insertion, ambient Unicode
database, dependency change, or modification of existing finite-domain APIs.
Unicode printability/width/title data must be exact or explicitly unsupported.
Direct surrogate-containing `str` cannot be materialized as valid UTF-8 and
must be represented as native codepoints or fail explicitly at the text writer
boundary, never repaired silently.

## Public encoding and conversion contract

`encode_python(&PythonValue, Format) -> Result<String, PythonValueError>` uses
Python's default `ensure_ascii=True`, `allow_nan=True`: it emits `NaN`,
`Infinity`, `-Infinity` rather than silently converting nonfinite values to null.
`encode_python_with_options` accepts the existing `Options`; false ASCII mode
returns a typed surrogate error whenever a preserved surrogate would otherwise
need invalid UTF-8. Individual adjacent surrogates remain two Python codepoints,
never combined into one astral scalar.

`DecimalInteger::parse` accepts `[+-]?[0-9]+`, canonicalizes all zero signs to
`0`, removes leading zeros and plus signs, and never parses magnitude through
f64. This is a typed constructor grammar, not Python's full int parser or a raw
JSON token insertion hook. `PythonString::from_codepoints` validates
U+0000-U+10FFFF, retaining surrogate values. `PythonDict::from_entries` replaces
duplicate values in their first position using exact codepoint keys.
`PythonValue::is_truthy` retains falsey empty/zero/None behavior and truthy NaN.

`try_to_json` fails before returning a value for an out-of-i64/u64 integer,
nonfinite float, or surrogate-containing key/value; no rounding, null fallback,
surrogate normalization or reordering. `try_from_json` preserves known finite
numeric kinds and signed float zero. It cannot recover information already lost
by an upstream serde conversion. These functions do not change `OutputInput` or
wire a native tree into a command consumer.

## Explicit safety exceptions

| Boundary | Inclusive limit and classification |
| --- | --- |
| Integer input | 4300 ASCII digit bytes before stripping zeros; `DecimalDigits` |
| Individual string | 1,048,576 Python codepoints; `StringCodepoints` |
| Dictionary construction | 100,000 input entries including duplicates; `DictEntries` |
| Native encode/conversion depth | Root0, children through128; `Depth` |
| Native encode/conversion nodes | 100,000 values including root, excluding dictionary keys; `Nodes` |
| Native encoded output | 16,777,216 UTF-8 bytes before each append; `OutputBytes` |

These are explicit native safety exceptions, not global Python configuration
changes or full equivalence to its configurable integer/recursion limits.
Constructors consume at most the bound plus the rejected input item. Callers
still own tree allocation and cannot infer a bound on already-allocated input,
`Clone`, `Debug`, `PartialEq` or destructor behavior from traversal limits.
No text or parsed-value result is returned on bound failure. The legacy
finite-JSON encoder's behavior/limits are unchanged.

## Independent execution

Initial gate-v2 receipt01 completed on 2026-10-26: acquired12:33:56.166Z,
released12:34:58.596Z, `mutexReleased=true`. It passed 35 native encoding cases,
four native Python guard tests, all 18 existing Unicode cases, original42
envelope cases and text83 source observations, core11+doc2 and CLI12+doc1
consumer checks, strict core Clippy, Black/mypy and workspace formatting.
This is real initial evidence, not a final release receipt: subsequent exact
output-byte and conversion-boundary assertions have been added.

The final rerun (receipt02) was refused **before validation started** on
2026-10-26T19:10:00.853Z: an unknown Python process was classified
`python_arguments_not_supported`. The known auth helper was separately allowed;
the unknown process was not guessed safe, killed or bypassed. `acquiredUtc=null`
and `mutexReleased=true` are retained. The final boundary tests still require
final execution. That refusal was retained while the slice was uncommitted;
the later source-candidate handoff below is not a passing release claim.
No prior source expectations were changed.

After the parent confirmed process33044 had exited, one final coordinated
attempt used the exact reviewed v3 launcher/module with a 120-second bounded
quiescence wait. Receipt03 again failed **before validation started**:
`Quiescence wait elapsed`, 51 scans, approximately120.1 seconds; last classifiers
were `python_arguments_not_supported` and `python_executable_not_trusted`.
`acquiredUtc=null`, `mutexReleased=true`; the failure receipt and prior passing
evidence are retained. No repeated attempt, process termination or classifier
exception was applied. The slice remained staged at that refusal rather than
inventing a passed final gate. Its original 20-minute acquired-time budget had only62.3
seconds consumed because neither refused attempt started the script.

The integration owner subsequently authorized a self-reviewed immutable source
candidate (message6087), with **final validation pending on isolated hosted CI**.
The feature owner's direct review covered the shared recursion/escaping/numeric
engine, all public validation/conversion paths, duplicate-key identity/order,
codepoint preservation, exact bound arithmetic, source fixture construction and
mutation guards. No new production defect was identified in that review;
this is not a replacement for compiling/running the final assertions.
Light formatting and patch checks are recorded separately from product tests.

### Required candidate checks

Run from `rust/` with the pinned toolchain and locked dependencies. The additive
CI hook runs `native_json`, the explicit `native_reference.py --encoding-only`
comparison and `test_native_reference.py` guards in the existing shared core/CLI
JSON stage. It retains the original envelope42 and Unicode18 comparisons and
also runs text83 there, so a core-only selection cannot omit the text regression.
Python setup already selects either core/output crate; the step runs on the
existing Windows/Linux/macOS matrix with UTF-8 configuration. Generic Cargo
tests alone are not native-oracle proof: the coordinator must record successful
execution of that named CI stage at the exact candidate head.

This CI hook is statically reviewed but **unexecuted** until a hosted run
completes; adding it does not turn the candidate's pending validation green.
The separate context/text stage and unrelated jobs/read stages are unchanged.
Equivalent focused commands are:

```text
cargo test --offline --locked -p fabric-core-json -p fabric-cli-output -- --test-threads=1
cargo clippy --offline --locked -p fabric-core-json --all-targets -- -D warnings
cargo run --offline --locked -q -p fabric-core-json --example native_json > <artifact-native>
python -B tests\compatibility\output\native_reference.py --encoding-only --observed <artifact-native>
python -B -m unittest discover -s tests\compatibility\output -p test_native_reference.py
cargo run --offline --locked -q -p fabric-core-json --example unicode_json > <artifact-unicode>
python -B tests\compatibility\output\unicode_reference.py --observed <artifact-unicode>
cargo run --offline --locked -q -p fabric-cli-output --example compatibility_probe > <artifact-output>
python -B tests\compatibility\output\reference.py --observed <artifact-output>
cargo run --offline --locked -q -p fabric-cli-output --example text_probe > <artifact-text>
python -B tests\compatibility\output\text_reference.py --observed <artifact-text>
cargo fmt --all -- --check
```

Use each runner's native path spelling and UTF-8 shell redirection; do not
normalize existing fixture bytes. Final additions include the exact16MiB output
and one-byte-over boundary, exact and over-depth/node conversions in both
directions, exact dictionary-input bound and signed NaN bit preservation.
Constructor limits and original35 source observations are not reduced.

The `native_json` public example emits observations only.
`native_reference.py --encoding-only --observed <artifact>` compares the complete
ASCII default/pretty0/2/4 strings, truthiness and Unicode-mode encoded result or
typed surrogate-failure classification to actual Python values and source dumps.
NaN bit patterns are constructed from IEEE754 bits; strings from exact Python
codepoints; integers from decimal strings. Unicode-mode failure is independently
established by strict UTF-8 encoding of Python's returned string.

`test_native_reference.py` guards wide-integer/nonfinite/order/surrogate losses,
pretty-layout proxies, truthiness changes and incomplete observations. Standalone
Rust tests exercise all constructor/conversion errors, native limits at exact and
one-past boundaries, invalid grammar/token injection, nested conversion failure,
signed NaN/zero preservation, and the same finite API byte regressions.
