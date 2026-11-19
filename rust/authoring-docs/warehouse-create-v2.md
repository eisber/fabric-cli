<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Opt-in native Warehouse creation

The experimental `fab-cli-v2` head mounts Warehouse creation only when built
with `headless,v2-create`. Default builds and v1 `fab-rs`/Python behavior do not
change. This slice is separate from Warehouse reads, definitions and management.

```text
fab-cli-v2 create /11111111-1111-1111-1111-111111111111.Workspace/Exact.Warehouse
fab-cli-v2 create /11111111-1111-1111-1111-111111111111.Workspace/Exact.Warehouse --input warehouse.json --wait
```

The workspace is an explicit GUID, not a discovered display name. The leaf is a
literal name, even if it looks like a GUID. There is no folder, tenant, catalog,
SQL endpoint, query, definition, sample-data or child-resource provisioning.
The host performs bounded complete item discovery for collision handling, then
at most one native `POST /v1/workspaces/{workspaceId}/warehouses`. Waiting uses
only the existing Fabric operation poll/result endpoints.
Discovery uses the generic Core `/items?recursive=false` route and preserves
that query across continuation pages. Within this explicit direct-workspace
scope, omitted or null `folderId` is root. Valid nested folder identities do
not match the root target; nested-only matches and malformed folder identities
reject before approval. A same-name root plus nested item selects only the root,
while multiple same-type root matches remain ambiguous. Completed create
results are rechecked for root scope; observed folder fields and field-presence
flags stay protected in receipt metadata, including submission evidence.

## Input

`--input FILE|-` uses the existing JSON grammar; no collation flag is added.
Input is optional. Omitted input, `{}`, and an empty `creationPayload` select
`Latin1_General_100_BIN2_UTF8`. The supported subset is:

```json
{
  "displayName": "Exact",
  "description": "Optional description, at most 256 characters.",
  "creationPayload": {
    "collationType": "Latin1_General_100_CI_AS_KS_WS_SC_UTF8"
  }
}
```

`displayName`, if supplied, must equal the destination name exactly. Only the two
collation strings above are accepted, case-sensitively. Input must be a strict
UTF-8 JSON object with unique keys, at most 4 MiB. Nulls, wrong types, unknown
fields, `type`, `definition`, `folderId`, sensitivity settings and SQL/query
content are rejected, including when an existing target would be skipped.
Names are preserved without normalization, must be nonblank and control-free,
fit the typed path grammar, and contain at most 256 characters. Provider naming
and capacity policies can additionally reject a request.

The native body always includes the exact `displayName` and selected
`creationPayload.collationType`, plus an explicitly supplied description.
No arbitrary source-file conversion or payload forwarding is installed.

## Policy, approval and evidence

The shared SDK planner owns `error` (default) and exact-existing `skip`.
Unsupported overwrite, duplicate and `--force` fail through SDK admission before
stdin/file input, credentials or provider effects. A unique exact name, kind and
parent are required for skip; ambiguous, case-different or wrong-kind matches
fail closed. Skip claims no equality between input and existing content.

The native endpoint documents `ItemDisplayNameAlreadyInUse`; this is the basis
for create-not-upsert support, not an atomic guarantee invented from a preflight
snapshot. A later provider conflict remains an error, never a successful skip,
overwrite or replay. Every execution requires immediate explicit approval from
the shared host. Nonterminal execution has no implicit approval or credential
fallback.

HTTP 201 can establish completion only with matching native identity. HTTP 202
means accepted, not completed. Resource IDs and operation IDs stay distinct and
outside `--project` data projection. Failed polling, local cancellation,
deadline expiry, projection failure, broken pipes and output races retain
available unprojected evidence and never replay or roll back the mutation.
Approval borrows the exact prepared method, bound URL and outgoing bytes,
including the default collation for omitted or partial input. Those same bytes
are submitted once; raw input alone is not the approval body. Protected
`meta.submission` retains the initial POST status, resource and operation
evidence separately from the latest `meta.observation` or `meta.httpStatus`.
Unexpected 201, 202 or 204 operation-result responses remain unknown,
non-completed `InvalidResponse` receipts with the original typed HTTP cause
retained; they are neither success nor a definitive provider rejection.
`--output` reuses the shared exact no-clobber sink. Live service, identity-policy,
SQL connectivity and cross-platform acceptance are not implied by offline tests.

The binding discloses anticipated metered Warehouse activity using
[Warehouse consumption](https://learn.microsoft.com/en-us/fabric/data-warehouse/usage-reporting),
source `c9d49756c2e184b61acde34d32f9aff63595db5b` (retrieved November 12, 2026).
The provider documents both user and system/background capacity consumption.
This disclosure claims no metered amount, charge, SQL execution or observed
resource outcome. It survives projection in `meta.anticipatedEffects`, and skip
declares no creation effects or outgoing request. The
[semantic-model contract](https://learn.microsoft.com/en-us/fabric/data-warehouse/semantic-models),
source `5815f8b8fe93f3cf508d7ba4af1f695972e865c0`, says default semantic models are
no longer automatically created with Warehouses. No child model is provisioned
or invented by this CLI.

## Source mapping and deliberate native field correction

The implementation baseline is `ced983722015e440e989f07d1bf866821d27fe90`.
`clients/cli/commands/mutations/src/create.rs` lines 521-528 select case-sensitive
Warehouse creation by default and the finite case-insensitive alternative for
legacy `enableCaseInsensitive`. The frozen Python recording
`tests/test_commands/recordings/test_commands/test_mkdir/test_mkdir_item_success[Warehouse].yaml`
records the typed warehouses POST and 202 response. These sources remain unchanged.

The [official Create Warehouse contract](https://learn.microsoft.com/en-us/rest/api/fabric/warehouse/items/create-warehouse),
retrieved November 12, 2026, was updated November 11, 2026 and identifies source
revision `edc068545c34ad50e4d543d40139cd360a3a2f7d`. It requires `displayName`,
documents the two finite `CollationType` values, optional description, 201/202
responses, operation headers and capacity/permission prerequisites.
Its `WarehouseCreationPayload` field is **`collationType`**, unlike the legacy
`defaultCollation` spelling. This native v2 schema preserves the default and
alternative choices but uses the current documented field; it does not claim
live proof that the legacy spelling is accepted. Broader optional API fields and
definition-based creation are outside this bounded binding.

The original lane consumed shared-create logical diffs
`e84426a`, `ce89c07`, `225903a`, `99dc963`, `9812bec`, `0454fb4`, `caf2f7f`, and
`0ec3848`, and `44576b0`, before the Notebook producer landed. The PR79 integration
update merges checkpoint `380322954dff38c28c886a12cd6a686372edc038` into original
Warehouse head `10dd5c74d6d8ae2e00d350751f37e8549b304bb6`. It preserves the
checkpoint's newer shared create host, SDK policy, strict-response decoder and
LRO implementation unchanged; only the Warehouse binding, registration and
contracts are added to those APIs. The executable selects Warehouse alongside
Notebook and Report, plus separately enabled KQLDatabase and Eventhouse.
Both ordinary and Livy-enabled root help retain every enabled owner disclosure.
There is no custom authentication, parser, policy, transport, LRO or output
engine. See [the shared host](create-host.md).

## Offline coverage

`warehouse_create_wire` connects the actual parser and shared preparation,
planner, approval and receipt paths to the real SDK HTTP adapter and scripted
responses. `warehouse_create_binary` runs the actual optional executable for
local admission, approval refusal, original input timeout and closed pipes.
The impact catalog registers both targets and the default/native feature
profiles on Windows, Linux and macOS. Registration and source checks are not a
claim that pending builds, native checks or independent review have passed.
Owned response regressions also require duplicate JSON keys, including escaped
aliases and nested duplicates, to fail closed in initial and final resources.
They retain bounded raw-response evidence and any separately valid initial
acceptance identity rather than trusting a last-key-wins interpretation. The
shared strict-response correction is consumed without changing the global
parser or importing the producer's resource tests.

### Executed local evidence

On November 12, 2026, Windows x86-64 at code revision
`c479a396bc9966a4ce8d050164aaa848b4227bff` executed 34 Warehouse wire tests,
7 executable tests, 9 generic create-host tests and 46 SDK create-policy tests:
96 passed, with no ignored or filtered tests. The native `headless,v2-create`
all-target check and strict Clippy passed. The metadata-derived default
53-package all-target check and strict Clippy, and the `v2-create`-only
all-target strict Clippy, also passed without suppression.

The sole authorized local lease ran from 15:59:21 UTC to 16:54:21 UTC, within its
original 16:59:21 UTC cleanup cutoff. Initial controller working-directory and
offline missing-dependency failures were retained; only the private cache was
restored, and the actual retest passed under the same lease and deadline.
The full metadata-derived impact run remained incomplete when broad default
test compilation reached the fixed work cutoff. Its timeout is not a full-plan
pass or frozen-oracle, feature-profile, Linux or macOS proof. The final update
after that revision changes documentation only; automated current-head CI and
independent frontier review remain separate acceptance requirements.

### November 19 integration update

The local Windows update against checkpoint
`380322954dff38c28c886a12cd6a686372edc038` used pinned Rust 1.99.1, one serial
Cargo process, two build workers, serial tests and session-owned artifacts.
Cargo metadata and the existing impact planner selected 60 packages and a
conservative full validation plan. All 60 package formatting checks, all-target
check and strict Clippy passed. Actual all-target check and strict Clippy also
passed for the registered `headless`, `v2-create`, `v2-create-headless` and
`v2-create-eventhouse-kql-headless` profiles, plus the metadata-derived union of
combined create, native runtime and documentation-search features.

The six shared SDK/LRO/workspace/mutation packages executed 974 all-target tests
and 7 doctests. The complete CLI all-target run with
`headless,v2-create,runtime-v2,docs-search` passed 1,108 tests, including all
34 Warehouse wire, 7 Warehouse executable, 9 shared host, 65 Notebook HTTP,
7 Notebook process, 36 Report, 16 feedback, 21 documentation and 21 Livy tests.
The architecture target separately passed 12 tests. Isolated Eventhouse and KQL
binary targets retained their original feature sets and passed 6 and 7 tests.
Combined-owner targets passed 101 tests, including 3 strengthened composition
tests, 30 Eventhouse, 27 KQL and the 41 Warehouse tests. The native Warehouse
binary build and all 126 impact-planner tests passed. These runs used complete
targets, with no ignored or filtered tests.

The broad check exposed an existing missing `BTreeMap` import in workspace SDK
unit tests; adding that test-only import restored compilation without changing
production SDK or identity behavior. Shared create, policy and LRO production
files remain byte-identical to the integration checkpoint. The session retains
the `pr79` metadata, full impact plan, source inventories, exact commands,
counts, exits and logs. The full impact plan's remaining optional profiles,
frozen-oracle checks, Linux and macOS matrix were not executed. This is local
provisional integration evidence, not fresh provider CI, independent review,
live-service, authentication-policy or release qualification.
