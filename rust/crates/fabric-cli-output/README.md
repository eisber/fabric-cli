<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI JSON output

`fabric-cli-output` is a pure, independently consumable **CLI-layer** library.
It translates the bounded Python JSON output contract at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; it is not full CLI parity.
It does not change `fab-rs`, its bootstrap argument errors, help, version,
namespace, or SDK behavior. There is no dispatcher or REPL integration.

```rust
use fabric_cli_output::{Indent, OutputEnvelope, OutputInput, Timestamp, parse_data};

let output = OutputEnvelope::new(
    Timestamp::parse("2026-01-02T03:04:05.000006Z")?,
    OutputInput {
        command: Some("get".into()),
        data: Some(parse_data(r#"{"displayName":"Synthetic"}"#)?),
        ..OutputInput::default()
    },
);
let semantic_tree = output.to_value()?;
let json_without_newline = output.to_json(Indent::Four)?;
let plan = output.render(Indent::Four)?;
let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
plan.write_to(&mut stdout, &mut stderr)?;
assert_eq!(plan.exit_code, 0);
assert!(stderr.is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
```

The timestamp is a validated caller value, never a clock lookup. Valid strings
use a real Gregorian year 0001-9999, UTC `Z`, seconds 00-59, and either no
fraction or exactly six fractional digits, as emitted by Python's constructor.
Callers own context, projection (including JMESPath), time acquisition, error
selection, stream handles, flushing, and process exit.

## Data and serialization contract

`json_dumps(&Value)` encodes raw definition/data values with Python's **default
no-indent** layout: comma-space/colon-space, ASCII escaping, insertion order,
and no newline. It does not produce an envelope, wrap data, or omit falsey data.
This differs from `Indent::Zero`, which remains pretty JSON with newlines.
`examples/definition_json.rs` demonstrates this standalone public API; its
independent source comparison is `definition_reference.py`. The existing numeric
domain and unsupported-runtime exceptions below apply unchanged.
Both layouts delegate to the single [core JSON encoder](../fabric-core-json/README.md);
the CLI public types remain compatibility wrappers. SDK callers use that core
crate directly, never depend on this CLI crate.

- A list is retained, even when empty. `None`/JSON null, false, numeric zero,
  empty strings and empty objects omit `data`; other values become one-element
  lists. Nulls and falsey values *inside* lists are preserved.
- Hidden strings and caller-resolved string enum values sort by Unicode code
  point, retaining duplicates. Missing hidden data is omitted; supplied empty
  hidden data emits `[]`. Mixed runtime object/string enum introspection is not
  supported.
- Only `None` omits messages/commands; empty strings remain. Success suppresses
  any error code. Failure defaults a missing/empty code to `UnexpectedError`.
  `OutputResult` uses `data`, `hidden_data`, `error_code`, `message` order;
  the envelope uses `timestamp`, `status`, optional `command`, then `result`.
- Unknown nested JSON fields and insertion order are preserved. The centralized
  serde_json `preserve_order` feature is required for this behavior, not cosmetic
  sorting; ordinary serde_json maps otherwise reorder Python dictionaries.
  The centralized `float_roundtrip` feature prevents parser precision loss.
- The semantic API returns a JSON tree. Byte serialization separately implements
  Python's `ensure_ascii=True`, lowercase UTF-16 surrogate escapes, colon-space,
  comma/newline layout, and integer indentation 0, 2 or 4. Indent 0 still emits
  newlines. Empty containers remain compact. `to_json` has no trailing LF;
  `render` appends exactly one LF, with no platform newline conversion.
- Numeric inputs are JSON null/bool, i64/u64 integers and finite f64 numbers.
  Integral floats retain `.0`, negative zero is retained inside lists, and
  exponents use Python's sign/two-digit spelling. Semantic numeric fidelity and
  fixture byte equality are separate claims: exhaustive Python dtoa compatibility
  across all f64 bit patterns is not established by these fixtures.
- `parse_data` rejects malformed JSON, out-of-range integer literals (rather than
  rounding them), unpaired surrogates and non-finite tokens. `finite_number`
  rejects NaN/Infinity before serde_json can silently turn them into null.
  A caller-supplied `Value` is already JSON-domain data; conversions that lost
  information *before* this API cannot be detected. Avoid unchecked
  `serde_json::to_value` on non-finite floats. Python's arbitrary-size integers,
  non-standard NaN/Infinity output, byte redaction, exotic objects, and general
  `Serialize` input coercion are not claimed here.

## Errors, streams and exits

| Public operation/source path | Stream | Exit | JSON fields |
| --- | --- | --- | --- |
| Success envelope / `print_output_format(..., json)` | stdout | 0 | Successful result |
| Failure envelope / ordinary decorated `FabricCLIError` | stdout | 1 | Failure and effective code/message |
| Exact code `Unauthorized` through the decorated error path | stdout | 4 | Same failure envelope |
| `CommandError::unexpected` / main unexpected-error handler | stdout | 1 | `UnexpectedError`; caller omits command as main does |
| `CommandError::cancelled` / main KeyboardInterrupt | stdout | 2 | `LongRunningOperationCancelled`; no command |
| Same cancellation code through ordinary command-error path | stdout | 1 | No special main-handler override |
| Serialization/input failure | no I/O | caller decides | Typed `OutputError`; no success-shaped fallback |
| Writer failure, including after a partial write | selected caller stream | caller decides | Original `io::Error`; no stderr fallback or forced exit |

`CommandError::new` uses Python's exact empty defaults (`UnknownError` and
`An error occurred while processing the operation`) and strips trailing periods
from the JSON message. It does not HTML-escape JSON. `unexpected(None)` uses
`An unexpected error occurred`, matching failure to extract exception text;
callers select Python's first exception argument/string before calling it.
`CommandError::invalid_json` matches `[InvalidJson] Invalid JSON format`.
The distinction between a failure envelope's `UnexpectedError` default and a
`FabricCLIError`'s `UnknownError` default is intentional.

Subcommands, headers, key/value/table flags, request IDs and verbose API details
are excluded from JSON by the source UI branch. This API accepts only the
already-selected error message/code, not SDK response parsing or decorated text.
These JSON APIs do not select text/table output; the separate pure text boundary
is described below. Real terminal framework/encoding behavior, arbitrary indentation,
query evaluation, configuration lookup and command routing remain unsupported.
`RenderPlan::write_to` never flushes, closes, retries, prompts or logs.

See the [source matrix](../../docs/output-compatibility.md) and
[independent oracle](../../tests/compatibility/output/README.md).

## Caller-injected text and tables

`text::render` selects JSON or text through `OutputFormat` and separate
`TextOptions`, leaving all existing JSON types/struct literals unchanged.
`text::TextPlan` holds ordered records so the hidden separator can go to stderr
while rows go to stdout. `write_to` preserves order, propagates the first I/O
error, and never retries/flushes. `TextError::partial_output` retains source output
before a later key/row render failure, for caller delivery before its error plan.

Table/header/key-value routing, source width scaling/wrapping, priority
truncation, Python-domain strings, success/notice/error display and
Fabric/OneLake/Azure text-only decorations have pure public helpers. Help/version
helpers take caller-provided strings and ordered categories, not a command
registry. The standalone `text_probe` exercises these APIs against actual
source definitions; no main/parser or family consumer is changed here.

Callers inject terminal width, East Asian character width, TTY/color palette,
newline and fallback policy. Unicode key/title and nested Python repr properties
are injectable formatters; defaults explicitly reject unsupported Unicode
properties rather than silently approximating them. This is not a terminal
framework or a complete Unicode database. See the precise
[text branch ledger and limits](../../docs/output-text-compatibility.md).
