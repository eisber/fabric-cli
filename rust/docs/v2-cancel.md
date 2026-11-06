<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Cancel an existing Notebook run

The experimental `v2-cancel` feature adds a root `cancel` command for an existing
Notebook scheduler run. It is an additive first slice, not a replacement for the
Python CLI or Rust v1 job commands. Default builds retain the existing grammar.

```text
cargo run --locked -p fabric-cli --features v2-cancel -- cancel --help
```

Remote execution additionally requires an explicitly configured `native-read`
or `headless` host. Enabling `v2-cancel` alone does not configure credentials or
transport. See [native host configuration](native-read-host.md) for the existing
host and platform restrictions.

## Address one existing run

```text
fab-rs cancel /<workspace-id>.Workspace/<notebook-id>.Notebook/runs/<run-id>
```

Each identifier must use the 36-character hexadecimal GUID form with hyphens.
The first slice accepts authoritative IDs only, not display names, a current
directory, wildcards, lists of targets, or arbitrary service URLs. This narrow
path does not establish a general v2 escaping or name-resolution contract.

The command requests cancellation through the public Fabric v1 scheduler:

```http
POST /v1/workspaces/{workspaceId}/items/{itemId}/jobs/instances/{jobInstanceId}/cancel
```

The request has no body. It does not submit a run, list jobs, modify a schedule,
delete a resource, or terminate a session. The caller must have the service's
execute permission and an appropriate delegated scope such as
`Notebook.Execute.All` or `Item.Execute.All`. Read permission alone does not
grant cancellation authority. The SDK uses the caller's injected Fabric
credential and bound endpoint; it never tries another account or audience.

## Acceptance is not terminal cancellation

A successful `202 Accepted` response means that the service accepted the
cancellation request. It does not prove that the run reached `Cancelled`, that
downstream work stopped, or that previous effects were rolled back.

The command returns a JSON acceptance receipt with the scoped run handle and
cancellation evidence. Diagnostics and unsuccessful outcomes use stderr and a
nonzero exit. `--format json` is the only supported command-specific format.
This first slice does not support `cancel --wait`, timeout flags, text output,
projection, or an output file.

The existing run can complete before the cancellation request, or complete or
fail after acceptance. Native errors such as `JobAlreadyCompleted` are retained,
not rewritten as successful cancellation. The command does not automatically
poll or replay the mutation.

Stopping local waiting, timing out, losing a response, or failing to write output
does not authorize a second cancellation request or imply rollback. Errors retain
the known target and whether the mutation may have been dispatched. A supplied
response Location must belong to the same endpoint and exact run; the command
does not follow a different job, operation, or service origin.

SDK errors retain the selected endpoint and bounded evidence from an actual
received response. An oversized response body is explicitly omitted, not
presented as complete evidence. CLI diagnostics project only allowlisted service
codes and correlation fields; arbitrary provider messages and object keys are
not copied to stderr.

## Capability limits

Environment publication, Livy statements or sessions, SemanticModel refresh,
SQLDatabase queries, Warehouse queries, other artifact types, and generic
control-plane operations are not supported by this first root-command slice.
Unsupported targets fail before credentials or remote work. There is no fallback
to deleting a Livy session, stopping an item, or issuing SQL `KILL`.

The existing v1 `job run-cancel` and run timeout policies remain unchanged.
The new root command does not reuse the v1 CLI timeout-cancellation policy.
It reuses the native SDK and the host's HTTP, identity, runtime, and limits.

## Offline validation

The SDK cancellation tests and CLI tests use injected credentials, transport,
signals, and writers. They do not authenticate to Fabric or mutate a tenant.

```text
cargo test --locked -p fabric-sdk-jobs --test cancellation
cargo test --locked -p fabric-cli --features v2-cancel --test cancel
cargo test --locked -p fabric-cli --features v2-cancel,native-read --all-targets
cargo test --locked -p fabric-cli --features v2-cancel,headless --all-targets
```

The dependency-aware validation graph includes default, opt-in, native-read,
headless, and combined-feature composition. Passing offline tests is not a
live-service, identity, tenant-policy, or cross-platform acceptance claim.

## Native API reference

- [Cancel item job instance](https://learn.microsoft.com/rest/api/fabric/core/job-scheduler/cancel-item-job-instance)
- [Get item job instance](https://learn.microsoft.com/rest/api/fabric/core/job-scheduler/get-item-job-instance)
