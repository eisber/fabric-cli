<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Table commands

`fabric-cli-tables` provides the `table` group and four typed command handlers
through `command()` and `TableCommand::from_matches`. Group-only invocation
selects help.

## Compose the host

`TableHost` consumes a table SDK, `TableResolver`, maintenance-job adapter,
execution, timestamp, configuration and optional text formatter. The library
does not acquire credentials or select a production host.

`run` writes progress to the supplied stderr writer and final output to the
selected stream. `execute` and `dispatch` return results without progress output.
The stderr writer must be `Send` for the asynchronous job observer.

`fabric_sdk_tables::JobMaintenance` connects maintenance handlers to the jobs
SDK. `SharedTableExecutor` delegates load requests to the shared HTTP engine.
`FabricLoadCompletion` reconciles the received acceptance through
`OperationSession::reconcile_accepted`; it never resubmits the load request.

`TableWithOneLake` composes storage listing, reads and properties with the table
executor. Storage and Fabric requests retain separate explicit audiences.

## Output and failures

`TableOutput` carries schema-header and job-ID key/value presentation intent.
Text selection requires a configured shared formatter before service work.
JSON uses the common CLI envelope.

`TableFailure` and `execute_detailed` retain SDK errors, accepted operation IDs,
job status and uncertain effects. `TableFailure::envelope` projects them only at
the presentation boundary. The older `execute` API provides a less detailed
command-error view.

Writer and observer failures propagate. Cancellation stops local work, not the
remote job; accepted work and earlier effects are never erased or replayed.

## Path and input policy

`ResolvedTablePath` represents a resolved OneLake child, not an item or OS path.
The host owns absolute, relative and home resolution, shortcuts and existence
checks. A load can suppress the final missing-table result, not arbitrary
metadata failures.

Maintenance validates the owning item's job support and uses its selected job
type. Retention uses signed decimal input with explicit digit limits. There is
no table `--dry-run` or general query option.

```text
cargo run --offline --locked -p fabric-cli-tables --example offline_consumer
```

The example uses injected credentials, HTTP responses and time; it does not
perform live service work.
