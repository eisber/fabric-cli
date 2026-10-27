<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# TABLE command composition

`command()` returns a Clap registration for the existing Python `table` group.
`TableCommand::from_matches` returns the four typed leaves; `None` denotes group
help, which the front door renders. This crate does not edit the central parser,
replace `fab-rs`, acquire credentials or choose a production host.

`TableHost` consumes a TABLE SDK, `TableResolver`, generic maintenance jobs,
execution, timestamp, configuration snapshot and optional shared text formatter.
`run` writes table-specific source progress to caller-owned stderr at the source
boundaries and final output to the selected stream. Writer failures propagate
without fallback, flushing or mutation replay. `execute` and `dispatch` are
programmatic result-only consumers; they deliberately do not write progress.
The separately owned generic job adapter must also provide its job diagnostics.

The released generic jobs SDK is now consumed by
`fabric_sdk_tables::JobMaintenance`, which can be injected directly as
`TableHost.jobs`. The real handler sends typed configuration through that adapter
to `JobClient` and the shared HTTP engine. Creation, timeout and known-state
progress are written at their source boundaries; `JobFailed` and service errors
retain their command-envelope classifications. Generic Failed/unknown-row table
formatting, log-warning formatting and the leading item-display-name job banner
still belong to the shared presentation/context integration and are not claimed
complete here. Service rows remain available in typed status events and errors.
`run` requires a `Send` stderr writer because its observer runs through the
generic SDK's `Send` future; it does not create threads or a new runtime.

`SharedTableExecutor` executes load POSTs through the actual shared
`HttpExecutor::execute_fabric` from prerequisite
`68e15d38bcc3df751b893414265876304a16a2f8`. It does not duplicate HTTP, TLS,
read retries, deadlines or response streaming. `FabricLoadCompletion` now
reconciles the actual received202 through the released
`OperationSession::reconcile_accepted(..., PythonSynchronous)`. It does not issue
another mutation, reuse Notebook payloads or copy an LRO loop. Poll/result tokens
come from the original explicitly bound provider, and the external POST counts
against finite operation limits. `TableOperationObserver` exposes the shared
engine's polling warnings; observer failures retain operation evidence.

`TableWithOneLake` in the SDK composes an existing generic OneLake executor with
this table executor. The generic storage SDK owns paging/read/HEAD, including
one storage token acquisition per page/request. The resource lane still owns the
generic OneLake-to-bound-HTTP adapter, now explicitly released as
`fabric-sdk-onelake-http::OneLakeHttpExecutor`. TABLE's consumers and standalone
example use that actual implementation, not a synthetic HEAD or copied adapter.
Only the final Azure network/credentials/time remain fixtures. The SDK crate does
not depend back on this CLI composition or the shared HTTP/facade dependency graph.

Lossless `OperationError` data, accepted ID and mutation uncertainty are retained
in SDK failures. All operation details are projected through the released
`fabric_cli_mutations::operation_error_output` helper. `TableFailure` and
`execute_detailed` retain the SDK failure until this explicit presentation
boundary. `dispatch` and `run` consume the already-normalized `OutputInput`
without constructing a second `CommandError`, preserving even intentional empty
messages after period stripping. `TableFailure::envelope` exposes the same pure
projection to consumers. The older `execute` method remains a lossy
`CommandError` compatibility view, not the exact normalized error boundary.

JSON uses `fabric-cli-output`. `TableOutput` exposes schema-header and job-ID
key/value presentation intent for the shared text renderer. If a host requests
text without configuring that renderer, it fails before service work.
The additive `TableTextOutput::normalized_error` delivers normalized failures
through that same shared renderer, without implementing another text formatter.

TABLE checks the resolved `OneLakeItem` through `FabricElement`, not `_BaseItem`:
the YAML item's advertised allowlist is not the actual OneLake command gate.
Valid resolved OneLake children can reach schema or the Lakehouse load endpoint
even under another owning item; the service decides whether a load is valid.
Maintenance separately checks the owning item's `job run` support after the
table progress notice and selects its real generic job type. For example,
Warehouse rejects `job run`, while SparkJobDefinition selects `sparkjob`.
`run_observed_for_job` preserves that source override rather than forcing
TableMaintenance everywhere. Shared context resolution still owns actual
absolute/relative/home paths, item FS_LS traversal, shortcut lookup and existence;
`ResolvedTablePath` must never represent an Item or OS path. A new load suppresses
only final not-found, not arbitrary resolver failures.

Retention uses signed decimal string division, Unicode15.1 decimal digits and
the source-oracle default4300-digit integer limit. `vacuum_with_digit_limit`
permits an explicitly supplied alternate/disabled bound without ambient runtime
inspection. There is no source TABLE `--dry-run` or general `--query` flag.

From the `rust` workspace, the standalone offline example composes public
registration, real handler, SDK, shared executor, a synthetic network and output:

```text
cargo run --offline --locked -p fabric-cli-tables --example offline_consumer
```

It uses only fixture credentials and a scripted202, Running and Succeeded. The
real public handler/SDK/OneLakeHttpExecutor/shared-HTTP/shared-operation path issues
one source HEAD, one POST and two GET polls before emitting completion. This is
offline execution of the actual adapters, not live Fabric/authentication proof.
See the [family matrix](../../docs/table-compatibility.md) for remaining gates.
