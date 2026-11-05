<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Ontology alias catalog and local argument validation

This CLI-layer package implements the independently testable local portion of
the selected seven-alias Ontology facade: create, delete, get, link, list, move
and update. It does **not** implement remote execution, a global registry, a
path resolver, an SDK, an MCP host or the complete Ontology feature.

`ontology_command` composes `operation_command` for each selected common verb.
`parse_common_args` and `parse_facade_args` use that same schema and produce
identical local arguments/errors. No second operation identity or specialist
alias is introduced. Parsed paths remain opaque until the accepted shared
resolver supplies canonical IDs. Local input/output are separate typed options;
parsing never opens a file, obtains credentials or dispatches remotely.

`bindings` and `select_binding` describe source-backed **proposals**, not enabled
operations. They distinguish public metadata, exact multipart definitions,
public minimized/expanded models, internal stored/expanded TMSL++ models and
native entity schema. Unknown type/generation and unsupported combinations fail
explicitly. A metadata discovery read can obtain generation before structured
representation dispatch. Generation must not be inferred from an item suffix.

Metadata patch, definition replacement and public/internal model authoring are
different body contracts. Native source binding and rename still need exact
released SDK types, bounded effects and accepted input contracts: link/move
parsing explicitly returns `UnresolvedCommandContract`, not executable success.
Other parser success only establishes valid local arguments, not complete
media/body/representation validation or permission to execute.

`validate_arguments` checks a resolved representation's input requirement,
metadata-patch/source-replace distinction, unsupported client conditions,
hard-only child deletion intent, unsupported recursion and actual LRO observation
applicability. Its success is local compatibility only; it does not read or
approve a body, reserve output, prove source permissions or enable dispatch.

Only JSON receipt format is admitted in this local slice. No raw bytes,
recursive get, directory extraction, implicit OWL conversion, docs/context
authoring, item-collection list expansion, model-backed execution, import,
export, serve, wait, plan or apply alias is installed. `--wait` is an option,
not a root command; a host must still reject it for synchronous-only bindings.
Unsupported revision conditions must also fail before remote work.

Numeric defaults in the captured common pages remain draft choices. The parser
therefore retains omitted budgets as `None`; it validates explicit positive
byte/record bounds and duration syntax/24-hour ceiling without choosing a
hidden host timeout. The coordinator must supply accepted finite policies
before execution.

## Required producer integration

The approved source baseline is
`eca98ab84c4a14d1ee158c2438e76dd77d2b0af0`. Its generic item SDK already recognizes
Ontology, but there is no shared v2 registry/path/receipt contract or MCP head.
Do not promote this CLI catalog into the SDK or infer operation support from
schema/help construction.

The coordinator must supply accepted neutral registration/target/effect/receipt
types and exact released public/native SDK commits. Runtime integration must
reuse injected credentials, transport, paging, operation reconciliation and the
caller runtime. No separate HTTP/auth/runtime/poller belongs here.

The host must validate strict UTF-8/JSON, duplicate/unknown fields, representation
media and input/response byte limits; resolve exact IDs/generation/native child
identity; atomically reserve no-clobber output; bind approval to exact effects
and conditions; then call the accepted SDK once. Receipts must distinguish
succeeded, accepted, partial, failed and unknown and retain original scoped
operation evidence on timeout or output failure. Output failure must not replay
remote work. These are pending integration obligations, not implemented tests.

## Source and spec evidence

Selected grouped Ontology root: `ab21dcdb-bf6f-453b-a0c3-760b41267d02` revision 18.
The seven children delegate to their common operation contracts. The active
grouped path contract leaves concrete escaping/namespace decisions open.

Backend source pin: `a5513205ddd862d450f14a6fdd14e78b40576719`.
PublicOntologyModelController distinguishes public forms and a 16 MiB write
cap. OntologyDefinitionController separates internal stored/expanded reads and
returns 501 for internal TMDL rendering. OntologyEntityTypesController provides
native V4 lineage-tag CRUDL and JSON Merge Patch. IOntologyScriptManager declares
transactional strict scripting; it is not permission for generic createOrReplace
under create. Source presence does not establish rollout or caller permission.

Local tests cover alias/schema equivalence, exact vocabulary, typed arguments,
numeric boundaries, representation separation and explicit unsupported states.
They do not establish argument-to-SDK execution, stdout/stderr receipts,
credential ordering, input/media limits or output writer/no-replay behavior.
Those tests require the producer/host integration and an explicit shared-host
validation slot. Workspace membership is discovered by the existing glob;
default-members, centralized internal dependencies and lockfile changes remain
coordinator-owned.
