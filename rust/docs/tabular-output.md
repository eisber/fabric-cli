<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared tabular file output

The Rust CLI supports shared CSV, JSON, and Parquet file output for the commands
listed here. Existing default JSON and text output remain unchanged without an
export request. Recorded validation covers the repaired codec, selected command
paths, actual local configuration exports, and receipt-output failures.

## Select a data file

Eligible table-producing commands use the same pair of options:

- `--format csv|json|parquet` selects the data-file encoding.
- `--output FILE` selects a filesystem destination.

For a tabular export, supply both options. The format is explicit, never inferred
from the file extension. Export does not silently overwrite an existing
destination, and binary stdout is not supported.

Choose a new file in an existing directory. Directory-only spellings, including
a trailing separator or final `.` component, fail preflight rather than becoming
file names. Paths with `..` components, linked parent directories, or Windows
reparse points are rejected; `--output -` does not select stdout.

The file contains only the dataset, not progress messages, diagnostics, or an
execution envelope. Stdout carries a common JSON receipt that distinguishes
command execution from file delivery. Diagnostics go to stderr, not into the
data file.

The existing `--output_format text|json` option selects normal command
presentation; it does not select a data-file encoding. Explicit
`--output_format` conflicts with the new export options, whose receipt is always
JSON. The existing `table load` input-format option describes the source being
loaded, not an export, and does not make `table load` a table-producing command.

The existing `acl get --output FILE` form keeps its prior behavior when
`--format` is absent. Adding `--format` selects the shared tabular export instead;
the wrapper owns file delivery rather than also requesting the older file-writing
path. This compatibility exception does not permit an unpaired export option
on other commands.

### Local configuration example

Local configuration export is confirmed with both default and `native-read`
builds of the Rust preview. The current Rust executable is `fab-rs`, as described
in the [Rust workspace guide](../README.md). The installed Python `fab` CLI does
not acquire these options from this work.

```bash
# Export local configuration without a Fabric request.
fab-rs config ls --format csv --output "local settings.csv"
```

The quoted relative filename works in Bash and PowerShell, including its space.
Configuration export needs no Fabric request or sign-in operation, and each
tested format contains the same 18 setting rows. The service-host-enabled build
also succeeds with invalid authentication settings, confirming that this local
path bypasses authentication. Run `fab-rs config ls --help` for the export options
or `fab-rs config --help` for the command group.

## Command and backend coverage

These are the current source-supported export consumers, not a claim that every
command, target, or host configuration has passed integration testing.
Remote commands retain their existing host, credential, and platform requirements.
The validation record distinguishes actual local exports from offline remote
adapters and compilation-only platform checks.

| Current consumer | Dataset boundary | Requirement |
| --- | --- | --- |
| `ls` (`dir`) | Complete declared listing rows | Configured remote host |
| `job run-list` | Complete declared job-instance or schedule collection | Configured remote host |
| `acl ls` (`acl dir`), `acl get` | Declared access-control collections | Configured remote host |
| `find` | Complete declared search records | Configured remote host |
| `label list-local` | Local label-definition records | Local configuration |
| `config ls` (`config dir`) | Local setting records | Local configuration |
| `table schema` | Actual declared column records, not table contents | Supported schema-reading host |
| `run` | Complete table returned by a prepared executable binding | Caller-injected tabular backend |
| `query` | Complete table returned by a proven read-only binding | Caller-injected tabular backend |
| Caller-owned generic `execute` parser | Complete table returned by a permission- and approval-checked command binding | Embedding frontend using `native_tabular::parse_args` |

The `run` and `query` roots and the caller-owned generic `execute` parser use a
shared caller-injected command host. They reuse the same export argument builder,
result validation, encoders, destination policy, and receipt delivery as the
other consumers. The shipped preview binary does not configure service drivers
for `run` or `query`: it returns a nonzero `NotConfigured` receipt before
reading input files or stdin, initializing configuration, or acquiring identity.
This is not a claim that SQL, KQL, DAX, or published-function service execution
has been implemented.

The integrated executable's existing `execute` root retains its
[native Dataflow query contract](dataflow-execution.md), including its real
provider, required `--wait`, named-query path and native Arrow result handling.
It is not replaced by the generic text-based frontend. Generic
`execute --text/--file` forms are available only to embedding frontends through
`fabric_cli::native_tabular::command` and `parse_args`; the shipped executable
rejects those incompatible forms as invalid arguments. Dataflow output continues
to use the separately documented [SDK tabular result contract](tabular-results.md).

`fabric_cli::tabular_export` exposes reusable `arguments`, `request`,
`validate_result`, and `deliver` APIs. The argument builder rejects conflicting
input/output options rather than replacing them; parsing and validation do not
run a backend. A producer supplies complete rows and any genuine empty schema,
while operation metadata stays in the receipt, not in dataset columns.
Providers with a native schema use `TableResult::from_table` and a validated
`TabularTable` for both empty and nonempty results. This preserves declared
unsigned types, all-null typed columns, and nullable columns even when the
returned rows contain no nulls. Conflicting raw and typed representations fail
instead of silently discarding either. JSON row inference remains available
for explicitly declared metadata producers without a native typed schema.

Only a declared table is eligible. A scalar, arbitrary response object, progress
record, or partial collection is not reclassified as a dataset. Single-file
delivery requires exactly one complete resultset; multiple resultsets fail rather
than silently selecting the first.

### Run, query, and execute host

The caller-owned parser accepts the same export pair for all three operations:

```text
fab-rs run <executable-path> --input parameters.json --format parquet --output result.parquet
fab-rs query <table-path> --text "SELECT ..." --format csv --output result.csv
```

The two examples above are configured-host invocation forms, not executable
service examples for the unconfigured preview binary. An embedding frontend
using `native_tabular::parse_args` can additionally parse
`execute <database-path> --file statement.sql --format json --output result.json`.
That generic form must not be passed to the executable's Dataflow `execute` root.
Caller-owned query and execute require exactly one of
`--text` or `--file`. Optional `--parameters FILE` selects native parameters.
`--file -` or `--parameters -` selects one provider-owned stdin reader; both
cannot read stdin in the same request. The provider decides whether a run target
permits omitted `--input`, and binds any requested wait or timeout behavior to
the actual backend. No input selector expands environment variables or
unescapes text a second time.

`fabric_cli::native_tabular::dispatch` receives an explicit `Backend`.
Preparation supplies a consumed-once `PreparedOperation`, proving the resolved
binding, input semantics, permissions, native tabular result capability, and
approval decision before execution. Query additionally requires a proven
read-only binding; text is opaque and is never rerouted to execute.
CLI flags cannot bypass approval or select an unadvertised engine.

This bounded host requires an explicit export request for configured execution.
Without that pair it rejects execution before effects rather than inventing
default rendering for new commands. Existing commands retain their existing
no-export output behavior.

Only complete results can produce a file. Accepted, partial, failed, and unknown
provider outcomes retain their native execution metadata without publishing
partial rows or inventing a waitable handle. A known multi-result operation
fails before execution; an unexpected multi-result response retains execution
evidence but fails delivery without discarding all but the first result.

The offline example `tabular_command_fixture` is a direct consumer of the
caller-owned `native_tabular::parse_args` parser and shared host, including
generic execute without shadowing the Dataflow root. From `rust/`, supply an
existing empty directory:

```text
cargo run -p fabric-cli --example tabular_command_fixture -- "<existing-output-directory>"
```

It explicitly configures a synthetic backend and exports two rows through each
root in all three formats, producing nine files and a receipt manifest.
It performs no service calls or input-file reads and is never a production
fallback. Independent JSON, CSV, and PyArrow readers verify the exported values,
including escaped strings, nulls, and signed/unsigned integer boundaries.
After building the example, the independent offline oracle can be run with
Python and PyArrow:

```text
python hosts/cli/fabric-cli/tests/native_tabular_oracle.py --example "<built-example-path>" --artifact-root "<evidence-directory>"
```

The oracle runs all nine parser/host exports, checks exact values and native
Parquet types/nullability, and retains the receipts and decoded-check evidence.
Python and PyArrow are development-time validation dependencies only, not a
requirement for the Rust CLI or its CSV/JSON/Parquet implementation.

## Rows, schema, and supported types

The shared codec lives in `fabric-cli-output::tabular`, with `TabularFormat`,
`TabularTable::from_rows`, and `encode`. Declared row adapters supply object rows;
the codec does not discover tables inside arbitrary command JSON. Column order
is deterministic: an explicit schema controls order, while `from_rows` collects
keys in first-seen order across rows, including later-only keys.
Missing fields become nullable cells, not omitted output columns.

Supported logical cells are null, boolean, signed 64-bit integer, unsigned
64-bit integer, finite 64-bit floating-point number, exact UTF-8 string, and
explicit logical JSON cells. Strings are not guessed to be numbers, dates,
or timestamps. Incompatible column types, non-finite numbers, invalid shapes,
and exceeded codec limits fail rather than producing truncated or rounded data.
Outside explicit JSON columns, integer/float mixtures are rejected instead of
losing integer precision.

Default inference can widen a nonnegative integer column to unsigned 64-bit
integers without changing its values. A column combining negative integers with
unsigned values beyond the signed 64-bit range fails.

Decimals, binary values, dates, timestamps, and other unlisted source types are
not universally supported. Adapters must reject unsupported source values, not
silently convert them to text or floating-point values. The codec cannot recover
precision already lost before a value reaches it.

### Logical JSON columns

Default `from_rows` inference keeps scalar column types strict and infers object
and array cells as logical JSON. Adapters can explicitly select JSON columns with
`TabularTable::from_rows_with_json_columns` or
`TabularTable::from_rows_with_json_columns_and_limits`. Selected columns accept
any bounded actual JSON value: strings, numbers, booleans, null, arrays, and
objects. Unknown or duplicate selectors fail; selection does not change
first-seen schema order or relax other columns.

JSON export retains the actual cell values; CSV and Parquet convert logical JSON
cells to compact JSON text without flattening them. Parquet records this
distinction with `fabric.tabular.logical_type` metadata: embedded Arrow field
metadata marks the cell type as `json`, and file metadata lists the JSON-cell
column names. This conversion is not support for arbitrary Parquet nested
structures.

In an explicitly selected JSON column, a present literal `null` is a nonnull JSON
cell, distinct from a missing key's SQL null. CSV writes the present JSON value
as the text `null`, while a missing key produces an empty field. Parquet stores
the former as UTF-8 text `null` and the latter as a physical null. JSON export
writes both as `null`, so the data file alone cannot recover that distinction.

The `config ls` adapter marks `value` as logical JSON, while `table schema`
marks present `type` and `metadata` columns. This preserves mixed configuration
values and scalar or structured schema descriptions without flattening them.
It does not convert arbitrary source types into supported values.

### Empty results

Zero rows with an explicit, nonempty schema are representable in all three
formats. CSV retains the header, JSON contains `[]`, and Parquet retains the
schema in a finalized file. JSON's empty array does not itself carry a schema.

An empty row list alone supplies no columns to `from_rows`. With no columns,
the codec produces empty CSV bytes or an empty JSON array for zero rows, but
rejects Parquet. Command adapters use genuine declared schemas for empty
unprojected listings and searches, table-schema records, supported ACL
collections, and job-instance or schedule collections.

Arbitrary empty projections, dynamic label results, and future producers without
a known schema do not acquire guessed columns. Parquet delivery then fails
explicitly with `schema_unavailable`; CSV and JSON can represent genuinely empty,
zero-column results. Missing label configuration fails validation instead of
producing a successful empty export.

### Resource limits

The codec's hard defaults are 100,000 rows, 1,024 columns, and 1,000,000
rectangular cells, including nulls. Modeled input storage and encoded output each
have a 64-mebibyte (MiB) limit; individual text, column-name, and compact JSON
values have a 1 MiB limit. Structured JSON depth is limited to 64, counting its
root as depth one. Library callers can lower these limits, not raise them; input
accounting is not a process-wide memory quota.

The input budget charges schema, row storage, all dense cells, and retained
payload before dense allocation and cloning. Synthesized null cells count
toward that budget even when the source rows are sparse.

## File formats

All formats encode complete values without terminal colors, display truncation,
or formatted table borders. They share the validated column order and preserve
Unicode. Encoding errors are failures, not successful partial exports.

### CSV

CSV uses a header row, commas, and carriage-return/line-feed (CRLF) record
endings. Fields containing commas, quotes, carriage returns, or line feeds are
quoted, with embedded quotes doubled. Embedded newlines and Unicode remain cell
data, not extra logical rows.

Outside logical JSON columns, null and an empty string both read as an empty CSV
field; CSV is not a typed, lossless replacement for JSON or Parquet. Logical JSON
cells use compact JSON text, including the null distinction described earlier.

Formula-looking strings retain their original data without hidden spreadsheet
sanitization. **Import untrusted CSV as text in spreadsheet software:** automatic
formula evaluation can run unwanted formulas, and automatic numeric conversion
can change identifiers or precision.

### JSON

JSON is an array of row objects, with keys written in schema order and null cells
written as `null`. The data file contains no execution or delivery envelope.
Readers must preserve integer precision and must not rely on every JSON consumer
retaining object-key order.

### Parquet

Parquet output is a real, finalized file, including its schema and footer, not
JSON or text with a different extension. It represents the supported scalar
types and nullability; strings remain strings, and logical JSON cells use the
text conversion described earlier. A zero-column schema is unsupported, and
file success is reported only after encoding and publication succeed.

## Failures and receipt evidence

Execution and delivery are separate outcomes. A destination failure found before
execution prevents the operation from starting. A later validation, encoding, or
file-writing failure returns a nonzero exit even when execution has succeeded.

Inspect the JSON receipt's execution and delivery outcomes before taking further
action. Execution success with delivery failure means that the operation already
ran, but no successful file delivery is established. The export path must not
rerun the operation or replay a backend request to repair encoding or writing.
Do not rerun an operation solely because file delivery failed.

Only complete, finalized data is published at the requested destination; partial
rows or a partial file are not reported as success. If writing the receipt to
stdout fails, the file can already exist, so a missing receipt is not evidence
that execution or delivery did not occur. Neither a receipt-write nor a
receipt-flush failure replays the operation or file delivery.

The host attempts a bounded `export_receipt_write_failed` diagnostic on stderr.
Its `retained_export` summary includes execution and delivery status,
destination, format, row/byte counts, and schema information when available,
not row data or arbitrary operation metadata. The diagnostic is limited to
16 kibibytes (KiB), up to 16 schema fields, and 1,024 path bytes, with explicit
path/schema truncation indicators.

The typed `ReceiptWriteFailure` retains the full original delivery receipt even
when the stderr summary is truncated or cannot be written. A receipt-output
failure returns a nonzero exit; it does not change successful execution or file
delivery into an unstarted operation. Inspect stderr and the destination before
deciding what to do next, and do not rerun an operation to recreate its receipt.

## Evidence limits and related contracts

The integration handoff records the actual results listed here. Independent
source review also verifies the repaired dense-memory preflight, directory-only
destination checks, and typed receipt-failure evidence.

| Check | Recorded result | Evidence boundary |
| --- | --- | --- |
| Shared codec | All 18 new tabular tests, existing output tests, and documentation tests pass | Tested codec values and public output contracts |
| Integrated command suites | `native-read,headless`: 233 tests pass across 13 selected library, binary, argument, export, family, job, table, and workspace targets | Offline adapters and executable failure paths; existing expectations retained |
| Final targeted rerun | 64 library, command-host, export, and delivery-failure tests pass after the final typed-result layout change | Includes all 16 real run/query/execute host tests |
| Default-build coverage | 48 selected default library/argv/export/delivery tests passed before the command-host addition; 14 initial command-host tests also passed in the default build | Final all-target checks include the new command hosts and example |
| Actual configuration exports | Default and `native-read` binaries each export JSON, CSV, and Parquet with 18 rows; Python CSV and PyArrow 26.0.1 independently decode matching values/schema; existing files are not overwritten | Local execution only; the service-host-enabled example also bypasses invalid authentication settings |
| Actual receipt failure | Closed stdout produces a nonzero exit, retains the published file, and reports execution/delivery evidence on stderr | Local executable; no operation replay |
| Strict linting | Both changed crates pass all-target Clippy with `native-read,headless` and warnings denied | Both native identity branches compiled; no live authentication |
| Linux cross-check | All CLI targets pass type checking for `x86_64-unknown-linux-gnu` | Compilation only, not Linux runtime behavior |
| Real run/query/execute hosts | Parser-to-host tests cover all three formats, policy checks, effect preservation and typed schemas; an explicit fixture backend exports nine files for independent decoding | Actual parser/host/output integration, not native service execution |
| Missing-provider frontdoors | Run/query return `NotConfigured` without creating output or initializing configuration; generic execute syntax is rejected rather than shadowing native Dataflow execute | Actual preview executable |

The recorded checks do not establish live service or authentication support, Linux
runtime behavior, complete cross-platform file behavior, or release readiness.
They do not supply new native service drivers for run, query, or generic execute,
and do not certify live Dataflow service execution.

See the [existing JSON output boundary](output-compatibility.md) and
[text output contract](output-text-compatibility.md) for normal presentation,
and the [Rust compatibility guide](compatibility.md) for host and command limits.
