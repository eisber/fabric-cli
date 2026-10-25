<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Rust compatibility and rollout backlog

The Rust workspace is an offline scaffold. It does not provide Python CLI,
MSAL, deployment, or complete cross-platform parity. The Python implementation
continues to own the existing `fab` product contract.

The [complete command inventory and crate ownership](command-parity.md) tracks
all 52 registered Python leaves, six groups, aliases, modes and current gaps.
SDK endpoints and passing read fixtures alone do not establish command parity.

## Bootstrap CLI contract

The temporary executable name is `fab-rs`, version `0.1.0`. There is no REPL,
embedded agent runtime, authentication command, deployment command or production
read host. Python-named `ls` (`dir`) and `get` are recognized; their public async
library composition is described in [read-command compatibility](read-command-compatibility.md).

| Invocation | Exit status | Standard output | Standard error |
| --- | --- | --- | --- |
| `--help`, `-h` | 0 | Human-readable help | Empty |
| `--version`, `-V` | 0 | `fab-rs 0.1.0` plus newline | Empty |
| `ls`, `dir`, valid `get` arguments | 1 | Empty | Sanitized `NotConfigured` JSON plus newline |
| No arguments, unknown commands or flags, `auth login`, `deploy` | 2 | Empty | One JSON error object plus newline |
| Output I/O failure | 1 | May be incomplete | Best-effort output-failure diagnostic |

Invalid invocations use this exact data shape and never include supplied values:

```json
{"error":{"code":"invalid_arguments","message":"Only --help, --version, ls, dir, and get are recognized in this preview."}}
```

Agents may rely on exit status, stream separation, and that error shape in
this bootstrap. Help text is for people, not a parsing interface. No successful
live service host or new resource/command namespace is established yet.
These temporary contracts do not replace Python's output and exit-code contracts.
Credentials are not accepted through command-line options.

## Required compatibility work

| Area | Compatibility gate before rollout |
| --- | --- |
| CLI | Preserve paths, context resolution, command behavior, and output/error contracts; review new namespace proposals separately |
| Deployment parameters | Native parameterization and logical/workspace-ID substitution |
| Item publication | Item-specific preprocessing and create/update selection |
| Planning | Dependency ordering, bulk publication, provisioning waits, and orphan unpublication |
| YAML | Case-insensitive duplicate detection and PyYAML typing, merge, and emission behavior behind a compatibility adapter |
| Transforms | Extended JSONPath mutation and Python regex replacement semantics; use advanced regex only for demonstrated cases |
| CLI projection | [Bounded native JMESPath wrapper](query-compatibility.md), distinct from JSONPath; numeric/runtime limits remain explicit |
| Pagination | Bounded traversal, validated continuation origins, and explicit incomplete results |
| Operations | Real operation handles and safe retry/reconciliation; accepted is not completed and cancellation is not rollback |
| Identity | Explicit account, tenant, audience, typed errors, and no silent fallback; enterprise policy evidence |
| Installation | Authentication and deployment without installed Azure CLI, Azure Developer CLI, or Python |
| Platforms | Native Windows/macOS/Linux execution and packaging evidence |

The [bounded Notebook deployment implementation](../crates/fabric-deploy/README.md)
is native Rust, not a permanent Python subprocess bridge. Broad configuration,
transforms, planning and Git behavior remain compatibility gates; the single
Notebook path is not a full deployment port.

## Validation boundaries

The additive [folder LIST/GET slice](folder-compatibility.md) has focused,
facade and shared HTTP consumers. It preserves raw folder/query semantics
separately from explicit typed-response, origin/path, cancellation and traversal
safety exceptions. It does not implement memstore tree assembly, folder CLI
handlers, mutations or live parity.

The bounded [generic item LIST/GET slice](item-compatibility.md) adds a focused
SDK crate, additive facade delegation and a shared HTTP adapter. Its source-pinned
offline oracle compares raw SDK request/row/error semantics separately from
Python-only quirks and explicit origin/path/bound/typed-response exceptions.
It does not replace the existing workspace goldens or implement CLI item
projection, definitions, mutations, specialized collections or live execution.

The scaffold has offline credential interoperability, typed identity errors,
single-page data serialization, subprocess CLI contracts, and workspace-edge
checks. Injected executors now exercise bounded workspace traversal and native
Notebook file preparation, create/update, operation polling and cancellation.
The shared `fabric-sdk-http` adapter now executes workspace and Notebook
requests through a caller-injected Azure HTTP client and timer. Its offline
tests cover streaming bounds, deadlines, read retries, cancellation, and
Notebook operation/result retrieval. Seven source-pinned Python HTTPS retry
traces are checked independently by
[`tests/compatibility/http/reference.py`](../tests/compatibility/http/README.md).
This is a bounded SDK execution slice, not CLI parity. The adapter's
[compatibility limits](../crates/fabric-sdk-http/README.md) explicitly retain
the no-mutation-replay exception and enumerate unported HTTP behavior.

The separately selected [native transport](../crates/fabric-sdk-transport/README.md)
now has offline Hyper/Rustls wire evidence with the actual SDK/shared executor,
bounded decoding, explicit trust/static proxy settings, cancellation and no
backend replay. The facade still has no default transport. These checks do not
test real authentication, live deployment, general enterprise trust/proxies or
broker policy.

The additive [capacity-list slice](capacity-compatibility.md) implements only
Fabric `/v1/capacities` through a focused crate, facade and thin shared HTTP
adapter. Its source-pinned offline oracle is separate from the unchanged
workspace/item/HTTP expectations. Bounded traversal, typed malformed-row
failures, cancellation and accurate progress are explicit SDK safety behavior,
not claims of Python CLI collection/output, ARM, authentication or live parity.

Native CI is configured for `windows-2025`, `ubuntu-24.04`, and `macos-15`.
Actual runner architecture is recorded by `rustc --version --verbose`; do not
infer execution or architecture from an OS label. Windows ARM64, Linux musl,
signing, notarization, and broker integration are separate gates.

## Research baselines

These are pinned reference points, not instructions to substitute current main
or claims that upstream code has been ported.

| Repository | Baseline commit |
| --- | --- |
| `microsoft/fabric-cli` | `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` |
| `microsoft/fabric-cicd` | `5ca096c00a9a878c646c59fe537b883eba951329` |
| `github/copilot-sdk` | `51e9556d2607f00f9f72038fa16accfe2f5d150d` |
| `Azure/azure-sdk-for-rust` | `c3c92ed4349125e25d176f13faa9ef016b790b3e` |

The initial released dependency resolution uses `azure_core` 1.1.0, `clap`
4.6.7, `serde` 1.0.229, `serde_json` 1.0.154, `thiserror` 2.0.21,
`async-trait` 0.1.92, and dev-only Tokio 1.54.1. `Cargo.lock` is authoritative.
Recheck versions, feature unification, native dependencies, and the declared
Rust minimum when changing the graph.
