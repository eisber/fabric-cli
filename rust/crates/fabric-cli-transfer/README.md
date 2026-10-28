<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Transfer command adapters

`fabric-cli-transfer` composes `export`, `bulk-export`, and `import` over the
released `fabric_deploy::definitions` APIs. It also registers the source `deploy`
arguments, but `TransferHost` explicitly rejects their execution. The central CLI
routes them separately to its bounded `fabric_cli::deploy::DeployHost`; neither
host claims the complete Python/CICD deployment pipeline.
There is no new executable, runtime, registry, credential provider, HTTP stack,
codec, item format, or deployment algorithm.

Python source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Rust base: `91e7523ea978c67ca183abaea04500e323c27eb3`.
This is family-library readiness, **not central front-door command coverage**.
The central host must mount the family and exercise actual production dispatch
with request, stream, and file effects before counting these leaves.

## Central host contract

1. Mount `transfer_subcommands() -> Vec<clap::Command>` in the existing root.
   Convert its root matches with `command_from_matches`. Preserve Python's
   multi-character `-tenv` by normalizing it to `--target_env` in the central
   lexical adapter; do not install a second parser or a fake public option.
2. Implement `TransferMetadata::resolve` with existing context/discovery and
   source command-support policy. Import allows a missing destination item;
   export requires an existing item or workspace. Supply complete workspace
   items or recursive folder items, preserving source traversal and support
   outcomes. Workspace single export also needs configuration-sorted selection
   names. A partial page is not a complete snapshot.
3. Construct `TransferHost` with the existing SDK `Client`, an injected
   `OperationExecutor`, file policy, UI, finite limits, and explicit CLI mode.
   The executor delegates to the existing shared HTTP/time implementation.
   Execute with caller cancellation on the caller's runtime. Override the
   `TransferFiles` asynchronous file capabilities for OS-backed hosts. The default
   methods retain existing synchronous injected adapters; they do not make those
   adapters nonblocking. The central CLI supplies `TokioTransferFiles` on the
   caller's existing blocking executor, not a second runtime or discovery callback.
4. Apply deferred `ImportCacheEffect` values after successful result delivery,
   matching the source handler. If output fails, retain the result as completion
   evidence but do not silently apply cache effects or replay the mutation.
   Use the actual returned create ID. A declined overwrite still returns the
   source existing-item cache upsert; an initial decline does not. Failures
   retain original operation IDs, uncertainty, possibly incomplete file paths,
   and earlier completed workspace-item writes. Never replay a mutation or a
   partial write on a generic failure.
5. Route `TransferUi` warnings through existing warning/logging policy and grey
   progress to stderr immediately. `render_result` and `render_error` use the
   shared JSON/text implementation, caller timestamp and terminal presentation.
   The command's explicit `--output_format` overrides the supplied configuration.
   `write_result` propagates stream errors without retry or success fallback.

SDK crates never depend on this CLI crate or on identity/deployment code.
The only workspace graph changes are this safe default member, its internal
dependency edges, and centralized path declarations. External dependencies,
versions, checksums and features remain unchanged.

## Implemented behavior and limits

| Leaf | Bounded behavior | Remaining source gaps |
| --- | --- | --- |
| `export` | Single item and selected/all workspace items; original prompt and warning; configuration display order versus discovery execution order; real shared definition GET/POST and local ordered writes; Notebook cleanup and format policy reused | Remote Lakehouse output and existing library's unsupported decoding branches |
| `bulk-export` | Required `--recursive`; existing local output check; nonempty-output prompt before sensitivity prompt; All/Selective requests with complete injected metadata; actual filtering, ancestor stripping and local writes; original type counts and summary | Caller discovery/support integration and remote output; no partial snapshot fallback |
| `import` | Initial/overwrite prompts; raw local deterministic traversal, cache-file exclusion and format policy; exact create/update body; actual completed ID and explicit cache effects | Environment readiness/publish lifecycle; remote input |
| `deploy` | Original arguments retained; `TransferHost` rejects execution, while central `DeployHost` separately consumes shared file capabilities | Full CICD processing remains outside this definition-transfer host |

`--all` affects workspace single export only; the source accepts it for an item
without changing single-item behavior. Transfer commands do not accept `--query`.
Single-item export retains Python's surprising success message on prompt decline,
but also returns `declined = true` and no file effects. That source message is not
proof that files were written. A workspace decline and bulk decline are silent.

`LocalTransferFiles` accepts relative directories beneath an explicit resolved
root, absolute directories contained by it, and optional caller-supplied tilde
expansion. Missing, empty, escaping, linked/reparse and unsupported paths fail.
It delegates file validation/read/write to the released local definition adapter.
The caller owns stable trees; this is not protection against hostile concurrent
replacement. Existing files are overwritten under the source confirmation rules;
no extra generic overwrite prompt or rollback is added.

Local path validation may run earlier than Python's post-definition-read check;
this avoids credential/service work for an invalid local root. Existing library
path/bounds/accepted-response safety exceptions remain unchanged. No Python
source, old oracle, baseline, or production containment rule is modified.

### Asynchronous local filesystem boundary (TD03)

The family now awaits directory resolution, bounded emptiness checks, complete
import preparation, preflight, directory creation and each ordered file write.
The released definition library still owns HTTP/LRO, codecs, source path
validation, Notebook cleanup, import sorting/exclusions and request metadata.
For export it first produces an in-memory write plan, then the family executes
the actual preflight/create/write phases asynchronously. A captured path is never
reported as written: preparation errors discard plan-only progress; failures
during actual writes retain completed paths and the possibly incomplete path.
Progress still follows successful preflight (bulk) or item-directory creation
(single), and preflight still precedes every write.

`fabric_cli::families::transfer_files::TokioTransferFiles` delegates to the
unchanged stable-root `LocalTransferFiles` policy on an injected Tokio handle.
Synchronous entry points on this adapter explicitly fail, rather than silently
performing blocking I/O. There is one queued/running blocking task per adapter,
including after a caller drops a future. The next call asynchronously waits for
that task's permit; no parallel tree walk, enumeration sort, runtime, global pool,
or new dependency is introduced. Native setup must await this adapter's `new`
instead of constructing synchronous `LocalTransferFiles`.

The configured definition bounds cap each phase: by default 4,096 entries/files,
10 MiB aggregate decoded/input bytes, depth 32 and 1,024 portable path bytes.
Import additionally retains the separate encoded-request byte check. Worker
import limits cannot exceed its constructor's caps. Export planning adds at most
one bounded decoded-byte copy while the released library is preparing the plan;
worker preflight takes ownership of that plan without another content copy.
Filesystem enumeration and source-defined import sorting are unchanged.

Cancellation is polled while filesystem work is pending and before the next
phase. Queued work checks a stop flag before touching the tree; import checks it
between entry/read calls. A running OS call cannot be interrupted. An in-flight
write may finish after cancellation returns, so its path remains uncertain,
earlier completed files remain present, and no cleanup, rollback, replay or
deadline-based no-effects claim is made. Each task has finite byte/traversal work,
not a promised OS completion time. Runtime owners must use bounded shutdown
(`Runtime::shutdown_timeout`, rather than ordinary runtime drop waiting forever
for blocked syscalls); abandoning the wait does not stop the OS operation.
Do not delete or reuse an owned test tree until its actual worker has exited.

#### Shared deployment consumer

The same native adapter also serves `DeployHost`. Its configuration/parameter
reads and complete Notebook repository preparation must use `read_async`,
`resolve_directory_async` and `prepare_repository_async`; invoking the adapter's
synchronous entry points is an error, not a deployment compatibility bridge.
The low-level synchronous `LocalTransferFiles` API and injected-adapter defaults
remain available unchanged for callers that explicitly own a suitable worker.

Repository preparation runs the released `NotebookRepository::discover` once on
the same serialized worker, retaining original listing order, containment,
preflight errors, universal-newline handling and complete-snapshot validation.
It returns only the bounded owned repository—not a mutex guard, borrowed local
capability, unchecked adapter clone or a partially loaded iterator. All local
preparation finishes before any service discovery or mutation. Configuration and
parameter reads retain their explicit YAML byte limits (default 1 MiB); repository
preparation retains the caller's aggregate Notebook bytes (default 10 MiB) and
entry bound (default 50,000), plus the existing per-directory/path checks.
No new sort, extra filesystem authority, service retry or file-write phase is
introduced.

Cancellation while preparation is pending drops its future, sets the same stop
flag and retains its permit until the worker really exits. Repository discovery
checks that flag between entry/read calls. There are no deployment mutations to
roll back during this phase; later publication retains its original completed IDs
and accepted-operation uncertainty. YAML/parameter parsing and transformation CPU
work remain their existing separately bounded implementation, not a claim that
the entire deployment pipeline has moved to a worker.

This consumer was omitted by the first TD03 mount: integration
`a62cd4028f2b3cad2107a293cb893fd07ba887f7` consequently failed 13 of the existing
18 `deploy_mount` tests before network work with `AsyncRequired`. The correction
must pass those unchanged tests alongside the current-thread transfer/read/
repository regressions. Source or formatting checks alone do not prove that gate.

## Independent source and focused checks

`tests/source_reference.py` extracts actual immutable Python handlers with AST,
injects synthetic API/context/UI capabilities, and performs real local reads and
writes. `source_receipt.json` was captured before executing Rust output. It pins
four cases: single export, recursive workspace bulk export, import create and
import update. This copies only synthetic artifact records, not private content.
The Windows receipt is immutable; Linux/macOS comparisons remove only documented
Windows text newlines and host separators in synthetic temporary-root displays.

From `rust/`, on the root-owned validation worker:

```text
python -B -m unittest discover -s crates/fabric-cli-transfer/tests -p test_source_reference.py -v
cargo fmt -p fabric-cli-transfer -- --check
cargo test --offline --locked -p fabric-cli-transfer --all-targets -- --test-threads=1
cargo test --offline --locked -p fabric-cli --features native-read --test async_transfer_files -- --test-threads=1
cargo test --offline --locked -p fabric-cli --features native-read --test deploy_mount -- --test-threads=1
cargo test --offline --locked -p fabric-cli --features native-read --test family_mounts root_export_and_bulk_export_write_real_definition_files -- --exact
cargo test --offline --locked -p fabric-cli --features native-read --test family_mounts root_import_worker_keeps_raw_bytes_file_before_directory_order_and_exclusions -- --exact
cargo clippy --offline --locked -p fabric-cli-transfer --all-targets -- -D warnings
cargo test --offline --locked -p fabric-deploy --test definitions --test notebook -- --test-threads=1
```

Tests use only synthetic credentials, scripted HTTP and exact owned temporary
directories. They do not authenticate, inspect caches/certificates, create live
resources, provision paid capacity, or execute jobs/Notebooks. Build scheduling,
central mounting, Linux execution and publication belong to the integration owner;
commands listed here are not a claim that validation has already passed.

The current-thread heartbeat regression queues the actual production file
adapter behind one gated blocking worker, runs a runtime heartbeat, checks that
the destination is untouched, then releases the gate and checks exact binary
bytes. It does not depend on disk speed, huge payloads or a timing threshold.
Separate tests cover dropped queued writes/permit retention, original native
errors, typed unavailable-worker errors, bounds, link rejection where supported,
ordered duplicate overwrites, partial writes and cancellation uncertainty.
