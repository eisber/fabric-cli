<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded CLI JSON output compatibility

The new `fabric-cli-output` crate translates a **pure output foundation**, not
full Python CLI parity. The authoritative Python pin is
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, with whole-source Git blob hashes
verified by the independent oracle. No Python product files or old fixture
baselines change. No consumer is wired into `fabric-cli/src/main.rs`.

| Pinned Python source | Ported behavior | Synthetic source cases / evidence |
| --- | --- | --- |
| `core/fab_output.py`, `OutputResult` | Truthiness/list wrapping; omission vs empty; hidden string/string-enum sorting | absent/null/false/0/-0.0/empty dict/string/list; retained list; enum/string duplicates/empty/Unicode |
| `core/fab_output.py`, `FabricCLIOutput` | Injected timestamp; status; optional command; success code suppression; failure default; exact key order | empty command, absent command, nested unknown data order, failure-all-keys, empty/default failure code |
| `utils/fab_util.py`, `dumps` | JSON-domain data; ASCII escaping; Python indent/separators; finite number spelling | four hand-authored byte fixtures (0/2/4 and a decimal tie), astral/DEL/control Unicode, f64 exponents/extremes and i64/u64 |
| `utils/fab_ui.py`, JSON success/error branches and `_safe_print` | Both streams are stdout; one newline under controlled headless collaborator; text/header/request-ID exclusion | actual source constructors/helpers; `ui-success`, `error-json-not-html` |
| `core/fab_exceptions.py`, `FabricCLIError` | Empty defaults; trailing periods; plain JSON rather than HTML/error text decorators | error-defaults/error-periods/error-json-not-html |
| `core/fab_decorators.py`, `handle_exceptions` | Ordinary error exit 1; exact `Unauthorized` exit 4 | decorated-unauthorized/case-sensitive/cancelled-command |
| `main.py`, unexpected/keyboard handlers | UnexpectedError exit 1 and absent command; extraction fallback; main cancellation exit 2 | unexpected/unexpected-fallback/cancelled-main |
| Public consumer boundary | Explicit render plan, byte payload, caller-owned Write; original partial-write error | standalone Rust integration tests and source-vs-Rust probe |

## Separate claims and intentional limits

Semantic comparisons assert the entire tree, including presence/absence of every
key, status, data/result, exit and stream. Byte comparisons use the actual Python
serializer and controlled headless print implementation, with LF checked
explicitly. No existing fixture is regenerated, normalized or weakened.
The Rust probe emits **observations**, never Python expected output.

The shared `fabric-core-json` library now owns the single string, number and
layout encoder. `fabric-cli-output` retains its public types and delegates to
that core implementation; SDK callers can use raw default-layout
`fabric_core_json::json_dumps` without a CLI dependency. The CLI's raw-data
wrapper preserves its `Result<String, OutputError>` signature. This is distinct
from envelope construction and from `indent=0`, which still emits newlines.
Twelve additional hand-authored default-layout cases cover separators, insertion
order, ASCII escaping and finite numeric boundaries. Native CI compares both
actual core and CLI example output with Python's serializer and the pinned
source helper, and preserves all 42 existing envelope comparisons. The exact
reviewed output-to-JSON-core dependency is guarded by positive and negative
architecture tests, not a general CLI-to-core permission.

Byte equality is demonstrated on the finite numeric cases, not every possible
f64 value or CPython dtoa tie. Arbitrary-size Python integers, non-standard
NaN/Infinity literals and unsupported Python runtime objects lie outside the
accepted Rust JSON domain. Invalid raw inputs fail with `OutputError::InvalidJson`;
they do not silently produce null/empty data. Python's `bytes` redaction is
documented and exercised as a source-only difference, not replicated by inventing
JSON runtime object conversion.

`preserve_order` is a required centralized serde_json feature because Python
retains envelope/result and nested dictionary insertion order. It introduces
serde_json's indexmap dependency closure; the lockfile records it explicitly.
`float_roundtrip` is centralized for correctly parsed finite f64 values.
No framework, terminal, logging, time, runtime, HTTP or auth dependency is added.

The library does not parse SDK errors, select or sanitize exception arguments,
resolve command/context paths, discover config, evaluate JMESPath, render text or
tables, or implement questionary/terminal fallback. The source's actual
questionary behavior across platforms is not certified by a headless fake.
The library never accesses an ambient clock, environment, mutable global, network,
filesystem, subprocess or logger.

The bootstrap `fab-rs` stderr/status-2 argument contract remains unchanged.
Product command consumers need their own review and end-to-end tests before using
this library. Windows local validation does not establish native macOS/Linux,
terminal behavior, release readiness or full CLI parity.

The independent oracle caught a decimal-midpoint regression during implementation:
Rust's standard float formatting emitted `2.9802322387695313e-08` where Python
emits `2.9802322387695312e-08`. The encoder now uses serde_json's already-locked
shortest-roundtrip formatter and separately applies Python's notation thresholds.
The new `numeric-rounding-boundary` fixture retains both real-source comparison
and manually pinned literal bytes to prevent recurrence without refreshing an
existing baseline.
