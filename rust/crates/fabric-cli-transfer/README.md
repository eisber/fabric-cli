<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Transfer command adapters

`fabric-cli-transfer` composes `export`, `bulk-export`, and `import` over the
released `fabric_deploy::definitions` APIs. It also registers the source `deploy`
arguments but explicitly rejects execution: the existing bounded Notebook
publisher does not implement the valid Python `deploy --config` pipeline.
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
   Execute with caller cancellation on the caller's runtime. Local file methods
   are synchronous; use an appropriate caller worker, not a blocking async
   service-discovery callback.
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
| `deploy` | Original arguments retained; explicit `NotSupported` without credential, file, metadata or service access | Full CICD config/YAML validation, override typing, environment selection, planning, publication and command-scoped bulk-flag cleanup |

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
cargo clippy --offline --locked -p fabric-cli-transfer --all-targets -- -D warnings
cargo test --offline --locked -p fabric-deploy --test definitions --test notebook -- --test-threads=1
```

Tests use only synthetic credentials, scripted HTTP and exact owned temporary
directories. They do not authenticate, inspect caches/certificates, create live
resources, provision paid capacity, or execute jobs/Notebooks. Build scheduling,
central mounting, Linux execution and publication belong to the integration owner;
commands listed here are not a claim that validation has already passed.
