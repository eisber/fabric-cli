<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Catalog commands

`fabric-cli-catalog` is the canonical CLI-layer owner of catalog search and hidden
resource discovery, a sibling of `fabric-cli-access` and `fabric-cli-api`.

## Belongs here

- `FindOptions`, `build_search_options`, `project_search` and `find`: source
  parameter/type normalization, request selection, row projection and diagnostics.
- `FindOutput` and `find_interactive`: bounded interactive paging with
  caller-injected output before continuation prompts, including EOF/decline.
- `HiddenNameKind`, `find_hidden_name`, `resolve_hidden_name` and
  `domain_name_by_id`: exact source hidden-name/ID resolution.
- `HiddenRead` and `read_hidden_metadata`: singleton/enriched metadata,
  retained internal values, source query behavior and warnings.

## Does not belong here

ACL/label mutations, generic resource operations, raw API request handling, mixed
command parsing, ambient caches, credentials and terminal ownership do not belong
here. The old mixed parser/host stays in
`../../compatibility/resource-commands`; this package never depends on it or on
another capability to obtain common CLI contracts.

`CommandError`, `CommandValue` and `Confirmation` reexport their single definitions
from `fabric-cli-command-contracts`. Callers inject SDK sessions, cancellation,
finite traversal/response limits, confirmation and output. Empty-page behavior,
cumulative summaries, diagnostics and prompt order remain source-compatible.
Declining continuation is not remote cancellation. No runtime, terminal,
credential or persistent context/cache is discovered.

## Provenance and verification

The implementation was moved intact from `fabric-cli-resources`; this extraction
does not redesign paging or query lifetime. The Python baseline is
`microsoft/fabric-cli@cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Independent expectations remain in `rust/tests/compatibility/catalog/`.
Package-local tests/examples remain in `fabric-cli-resources` at
`../../compatibility/resource-commands`, including interactive, metadata,
bindings and query-dispatch contracts through the original imports. Its
extraction-identity test also uses this owner directly. These are offline
contracts, not live authorization, persistence or terminal-platform acceptance.
