<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Unicode output tests

These tests verify the output library's complete Unicode scalar database,
source-compatible text formatting and separate Rich terminal-width policy.
The development-time generator produces property ranges, full case mappings,
independent whole-text expectations and license notices. The library embeds
reviewed generated data and does not require Python at runtime.

## APIs covered

- `production_unicode::embedded_provider` returns immutable shared data or the
  original initialization error.
- `production_unicode::width_provider` returns the existing scalar-width trait
  implementation with typed text initialization errors.
- `production_unicode::key_formatter` and `value_formatter` provide bounded
  function-pointer callbacks for `text::TextOptions`.
- `ProductionUnicode::properties` and `codepoint` query every valid scalar.
  `icu_properties` exposes current ICU data separately from source-version
  compatibility properties.
- `source_text_width` measures additive East Asian F/W scalar width.
  `rich_text_width` measures Rich's distinct scalar and ZWJ/VS16 sequence policy.

Source formatting uses Unicode 15.1.0, Rich width uses an explicit Unicode
17.0.0 table, and current ICU properties use Unicode 18.0.0. The policies are
intentionally separate. Combining marks and controls do not acquire zero width
in the source scalar-width policy. Surrogates are not valid Rust scalar inputs;
noncharacters and unassigned scalars remain valid and are covered.

## Run tests

The generator requires CPython 3.13.15 and Rich 14.3.3. Run commands from `rust\`.
Use an isolated development environment with the pins in `requirements.txt`.
Full generation and Cargo checks require the shared host's validation slot.

```powershell
# Small offline safety checks; no full-scalar sweep.
python -B -m unittest discover -s hosts\cli\fabric-cli-output\tests\production_unicode -p test_generate.py
python -B hosts\cli\fabric-cli-output\tests\production_unicode\generate.py

# Additive generation refuses existing artifact destinations.
python -B hosts\cli\fabric-cli-output\tests\production_unicode\generate.py --generate --full-scalar-slot
python -B hosts\cli\fabric-cli-output\tests\production_unicode\generate.py --verify --full-scalar-slot

cargo test --offline --locked -p fabric-cli-output production_unicode -- --test-threads=1
cargo test --offline --locked -p fabric-cli-output -- --test-threads=1
```

The native scalar test checks all 1,112,064 valid scalars against the independently
generated property checksum. Whole-text tests cover casing, repr, actual table
rendering, malformed sequences, typed errors, immutable initialization and
resource limits. Successful smoke checks are not complete-domain evidence.

## Supported limits

Formatting is limited to 4 MiB aggregate input, 16 MiB output, depth 128 and
1,000,000 visited values and keys. Callers may only tighten these limits.
Property data is limited to 4 MiB, 32,768 ranges, 8,192 full case mappings and
512 Rich widening scalars. Each development-time full sweep has a 45-second
deadline and produces JSON artifacts no larger than 4 MiB.

`TextError::UnicodeResource` identifies the exhausted budget.
`TextError::UnicodeData` identifies invalid initialization data. Neither failure
falls back to ASCII or is reported as an invalid key or JSON output value.
Existing source key-validation errors propagate unchanged.

The scalar-only width trait does not enforce whole-table allocation budgets;
the rendering caller must admit and budget its complete input.

See the [Unicode contract](../../../../../docs/unicode-production-contract.md)
for provenance, dependency and license evidence, and compatibility decisions.
Keep `THIRD_PARTY_NOTICES.txt` and generated `PYTHON-LICENSE.txt` with the data.
