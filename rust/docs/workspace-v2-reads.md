<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental v2 workspace reads

The `workspace-read-v2` feature adds a separate `fab-cli-workspace` binary. The existing
Python `fab` and Rust `fab-rs` commands retain their contracts. This slice is
limited to primary workspace metadata and one bounded workspace-items page.
It does not implement the complete v2 command surface.

## Commands

Use an explicit canonical workspace UUID. Names, relative paths, wildcards,
implicit current-directory selection and other resource types are unsupported.

```text
fab-cli-workspace get /11111111-2222-3333-4444-555555555555.Workspace --max-bytes 1048576 --timeout 30s
fab-cli-workspace list /11111111-2222-3333-4444-555555555555.Workspace/items --limit 100 --max-bytes 1048576 --recursive false --timeout 30s
```

`list /<uuid>.Workspace` addresses the same items collection as
`list /<uuid>.Workspace/items`. The matching facade spellings use the same
request, output, permission and coverage contract:

```text
fab-cli-workspace artifact workspace get /11111111-2222-3333-4444-555555555555.Workspace --max-bytes 1048576 --timeout 30s
fab-cli-workspace artifact workspace list /11111111-2222-3333-4444-555555555555.Workspace --limit 100 --max-bytes 1048576 --recursive false --timeout 30s
```

The numbers in these examples are **explicit choices, not defaults**. This
slice requires `--max-bytes` and `--timeout` on both reads, plus `--limit` and
`--recursive true|false` on list. Budgets must be positive. Timeout syntax is a
positive integer followed by `ms`, `s`, `m` or `h`, at most 24 hours.

Only JSON is advertised. `--format json` is optional and does not change the
representation. `--project EXPRESSION` (or `-q`) applies the existing JMESPath
interpreter to **data only**, retaining JSON scalar types. An explicit empty or
invalid expression fails; it does not select legacy path inventory. Projection
syntax is checked before remote work, has a 4-KiB expression safety ceiling,
and does not bypass the pre-projection data budget.

`--output FILE` (or `-o`) selects one explicit local destination; omitted output
or `--output -` selects stdout. Existing destinations fail before credential
setup. Local file output uses a checked caller-owned parent, a sibling temporary
file and atomic no-clobber hard-link publication. A racing destination is never
overwritten. Unsupported filesystems, unsafe names/links, partial writes and
cleanup errors fail explicitly; there is no overwrite fallback. Parent
directories must be stable and caller-owned. This is not a sandbox against
hostile concurrent directory replacement.

No `--profile`, `--continuation`, unbounded `--all`, generic filter, definition
format, folder union, recursive package, enrichment or admin option is exposed.
Unsupported syntax fails instead of silently selecting a different capability.

## Build and host boundaries

From `rust/`, select the binary explicitly:

```text
cargo run --offline --locked -p fabric-cli --features workspace-read-v2 --bin fab-cli-workspace -- --help
cargo run --offline --locked -p fabric-cli --features workspace-read-v2 --bin fab-cli-workspace -- --version
```

These display-only commands do not configure an account, open an output file or
connect to a service. Without a native host, valid remote reads fail
`NotConfigured`; there is no successful no-op.

For the existing Windows native host, combine `workspace-read-v2,native-read`.
The existing explicitly configured unattended host can instead be selected with
`workspace-read-v2,headless`. See [native host configuration](native-read-host.md)
for the already-supported identity inputs and platform restrictions.
The new binary reuses that host's silent credential selection and shared HTTP
executor; it does not add login, consent, a credential chain or a second HTTP
stack. It does not initialize navigation or discover other workspaces to resolve
an explicit UUID. Custom service endpoints must be supported by the selected
binding; they never fall back to the public endpoint silently.

`--timeout` is the caller's command deadline, not a remote cancellation request.
The native composition includes credential setup and service execution in that
deadline. Library callers inject cancellation and deadline futures and own
output delivery. Synchronous local filesystem/stream calls cannot be forcibly
interrupted; an output failure or incomplete delivery is not a successful file
claim. No timeout or output error resubmits a read as another identity or route.

## APIs and permissions

| Read | Native request | Authorization boundary |
|---|---|---|
| Primary workspace metadata | `GET /v1/workspaces/{workspaceId}` | Viewer or higher workspace role; delegated `Workspace.Read.All` or `Workspace.ReadWrite.All` |
| Items | `GET /v1/workspaces/{workspaceId}/items?recursive=true|false` | Workspace Viewer; delegated `Workspace.Read.All` or `Workspace.ReadWrite.All` |

Both use the caller-injected Fabric audience. Native SP/MI support still
requires the applicable tenant and resource permissions. Source inspection,
offline fixtures and a configured credential do not prove live authorization.

Primary get makes no role-assignment, private-endpoint, Spark, Git or admin
request. Metadata returned by the service, including API/OneLake endpoint
properties, is data rather than permission to select another origin.
Item rows preserve unknown fields and source order. No create, update, delete,
placement, workload execution, reset, restore or principal grant is performed.

Official references:

- [Get workspace](https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/get-workspace)
- [List items](https://learn.microsoft.com/en-us/rest/api/fabric/core/items/list-items)
- [Identity support](https://learn.microsoft.com/en-us/rest/api/fabric/articles/identity-support)
- [Pagination](https://learn.microsoft.com/en-us/rest/api/fabric/articles/pagination)

The source basis is Rust checkpoint
`eca98ab84c4a14d1ee158c2438e76dd77d2b0af0`, including upstream Rust source
`a9df45501ec53d921c6fc765c726c32979b85931`. The selected workspace specification
is `58aa99f3-22cc-45c3-afb3-14336bc5aead` revision 11 with its owned get/list
pages and grouped contracts. Existing v1 independent-oracle pins and expected
outputs are unchanged. This additive v2 contract is not a Python-parity claim.

## Bounds, coverage and output

The SDK uses existing `ApiClient` authentication/HTTP and shared `TokenPager`
continuation validation. It reads at most one native items page, under the
existing finite response and row safety ceilings, and **never requests page two**.
The caller's output bound is different: compact UTF-8 JSON data bytes before
projection, excluding envelope/metadata, with brackets, commas and escaping
included. Only whole rows are emitted; if the first row or empty array cannot
fit, the read fails `BudgetExceeded`. Metadata is never clipped.

There is deliberately **no resumable cursor in this first slice**. If the
service has another page, or the record/byte limit omits a returned row, the
result is `partial`, nonzero, `complete:false`, `truncated:true`,
`continuation:null` and `resumable:false`. No raw next-page token can skip omitted
rows or masquerade as an identity-bound v2 cursor. Raising a bound cannot make
the initial page exhaustive when the service advertises another page.

The envelope uses `schemaVersion: "0.1"`, `status`, `data`, `meta` and `error`.
Unprojected list data is always an array, including `[]` and singleton lists;
projection can select a scalar. Target, explicitly selected nonsecret context,
representation, provenance, observation time and coverage are not projected.
Unknown completeness is null, not true.

Complete results return exit 0. Failed or partial results return nonzero.
Invalid/unsupported inputs return exit 2. Failed reads write an error envelope
to stderr and do not publish the selected result file. Partial data retains its
labelled envelope on the selected output and a nonzero exit. File/stdout errors
retain the known target and observed result without issuing another request.
Neither HTTP 202 nor malformed/missing primary/list data is treated as an empty
successful result.

## Offline validation surfaces

New SDK cases live in `fabric-sdk-api/tests/workspace_reads.rs`; integrated
parser, SDK/HTTP, projection and output cases live in
`fabric-cli/tests/workspace_v2.rs`. The scalar-preserving query entry point has
separate cases alongside unchanged legacy search assertions.

After obtaining the shared-host validation slot, the focused commands include:

```text
cargo test --offline --locked -p fabric-sdk-api --test workspace_reads
cargo test --offline --locked -p fabric-cli-query
cargo test --offline --locked -p fabric-cli --features workspace-read-v2 --test workspace_v2
cargo check --offline --locked -p fabric-cli --features workspace-read-v2,native-read --all-targets
```

Use the repository's metadata-derived impact plan for affected consumers,
features and platform checks; these commands alone are not integration or live
acceptance evidence. No live tenant, native authentication, cross-platform
service execution or complete CLI v2 acceptance is asserted by this document.
