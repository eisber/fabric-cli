<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI output

`fabric-cli-output` builds structured result envelopes and text presentation
plans. It performs no service work, clock lookup, credential discovery or
process exit.

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
let plan = output.render(Indent::Four)?;
let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
plan.write_to(&mut stdout, &mut stderr)?;
assert_eq!(plan.exit_code, 0);
assert!(stderr.is_empty());
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Structured output

The envelope contains `timestamp`, `status`, optional `command` and `result`.
Result fields are ordered as `data`, `hidden_data`, `error_code` and `message`.
Timestamps are validated UTC values supplied by the caller.

Lists remain lists, including empty lists. Null, false, zero, empty strings and
empty objects omit top-level `data`; other values become one-element lists.
Falsey values inside lists are preserved. Empty supplied messages and commands
remain distinct from missing values.

Success suppresses error codes. Failure defaults a missing or empty code to
`UnexpectedError`. Hidden names are sorted by Unicode code point without
deduplicating.

`to_value` returns the semantic JSON tree. `to_json` serializes without a final
newline; `render` appends one line feed. Integer indentation supports 0, 2 and
4 spaces. Raw `json_dumps` uses compact containers with comma/colon spacing,
ASCII escapes and insertion order rather than an envelope.

## Data validation

The supported JSON domain includes i64/u64 integers and finite f64 values.
`parse_data` rejects malformed input, oversized integers, unpaired surrogates
and nonfinite tokens. `finite_number` rejects NaN and infinity explicitly.
Values already converted lossily by a caller cannot be recovered.

Serialization delegates to the shared
[core JSON encoder](../../../infrastructure/core/fabric-core-json/README.md). SDK consumers use that crate
directly instead of depending on CLI output.

## Text and streams

The text renderer supports notices, tables, key/value layouts and command
presentation. Callers supply formatting options, width measurement and writers.
Keep output plans until delivery completes: a partial write is an error, not
permission to retry the associated operation.

## Unicode text

The default text callbacks use an immutable embedded property database for full
Unicode key casing and Python-compatible nested string representation. Obtain
the source scalar-width implementation explicitly before rendering a table:

```rust
use fabric_cli_output::production_unicode::{key_formatter, value_formatter, width_provider};
use serde_json::json;

let widths = width_provider()?;
assert_eq!(widths.source_text_width("e\u{301}")?, 2);
assert_eq!(key_formatter("état_id")?, "État ID");
assert_eq!(value_formatter(&json!(["\u{200b}"]))?, "['\\u200b']");
# Ok::<(), Box<dyn std::error::Error>>(())
```

Source formatting uses Unicode 15.1.0. Its table width is additive East Asian
F/W width: combining marks, controls and joiners still count one. The separate
`rich_text_width` API uses Rich's Unicode 17.0.0 scalar and ZWJ/VS16 sequence
policy. `icu_properties` exposes current Unicode 18.0.0 properties independently.
These policies are not interchangeable, and source wrapping remains scalar-based.
All valid scalars, including noncharacters, private use and unassigned values,
are covered; integer surrogate inputs are rejected.

Initialization errors remain explicit. `TextError::UnicodeData` identifies
invalid provider data and `TextError::UnicodeResource` identifies an exhausted
budget. There is no ASCII fallback, environment-selected database, mutable
per-command configuration or Python runtime dependency. The explicit legacy
`text::title_key`, `text::python_string` and `text::ascii_width` helpers retain
their ASCII-only contracts for callers that select them.

Formatting is bounded to 4 MiB aggregate input, 16 MiB output, depth 128 and
1,000,000 visited values and keys. Callers may tighten these limits. The scalar
width trait cannot enforce whole-table allocation limits; render callers must
budget their complete inputs. JSON encoding and numeric domains are unchanged.

See the [Unicode tests](tests/production_unicode/README.md) for validation
commands and supported limits, and the
[Unicode contract](../../../docs/unicode-production-contract.md) for provenance,
licensing and compatibility decisions.
