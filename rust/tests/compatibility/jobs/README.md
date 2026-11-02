<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Job command contracts

This suite verifies all eight job commands with synthetic HTTP, time, UI and
configuration. Fixtures are immutable; native results never generate expected
values.

| Runner | Contracts |
| --- | --- |
| `reference.py` | Execution data, schedules, routes, output and polling |
| `branches_reference.py` | Resource support, parameter and status branches |
| `command_source.py` | Command-specific raw response interpretation |
| `poll_source.py` | Non-string states and non-200 polling responses |
| `numeric_source.py` | Checked numeric spelling and decimal-character inventory |
| `endpoint_reference.py` | Public, Daily and MSIT routing with fixed audiences |
| `operation_output_reference.py` | Shared operation errors and retained evidence |
| `skill_headers_reference.py` | Explicit header propagation and isolation |

From the repository root:

```text
python -B rust\tests\compatibility\jobs\reference.py
```

The repository validation plans run the related references and native consumers.
The native path includes parser, command session, jobs SDK, shared HTTP and
operation reconciliation. Unexpected requests and mutation replay fail tests.

Failed or unknown states, cancellation, timeout policy, confirmation and silent
responses remain distinct contracts. Header and endpoint safety checks occur
before mutation. These are offline checks, not live credential or tenant-policy
acceptance.

Branch ledgers record observed decisions and inventory, not blanket coverage or
whole-command completion percentages. Keep new observations separate from fixed
expectations and preserve unsupported-domain classifications.
