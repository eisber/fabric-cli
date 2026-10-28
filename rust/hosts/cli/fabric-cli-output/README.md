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
