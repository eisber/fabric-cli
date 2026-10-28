<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Read query argument contracts

This suite measures raw argv separately from already-constructed handler input.
LS accepts a scalar query; GET accepts joined variadic query tokens.
Split query-looking tokens must not be silently reassigned.

Fixed fixtures check attached/separate values, option stops, repeated-option
precedence, aliases, negative numbers, `--`, path interaction and absent versus
empty queries. Consumer cases connect those parsed values to actual query and
output handling.

From `rust/`:

```powershell
cargo build --offline --locked -p fabric-cli --examples
python -B tests\compatibility\query_arguments\reference.py --rust-probe target\debug\examples\read_probe.exe --folder-probe target\debug\examples\folder_read_probe.exe
python -B -m unittest discover -s tests\compatibility\query_arguments -p test_reference.py -v
```

Use executable paths appropriate for the host and the required shared-host
resource grant. Omit both probe arguments for reference-only execution.

Probes receive argv, not expected values. Both execute real parser/query/output
code, and the folder probe uses the shared HTTP adapter with synthetic transport.
Guard controls detect drift, reordered inventories, corrupted requests/streams
and forbidden file, network, process or sleep activity.

The inventory is bounded lexical coverage, not a guarantee for every parser
extension. Handler-list query fixtures remain separate from scalar argv cases.
