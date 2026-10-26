<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access, catalog and raw API frontdoor adapter

Source authority is unchanged Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`: ACL/label/FIND/API registrations,
handlers and `process_nargs`. The implementation base is released
`91e7523ea978c67ca183abaea04500e323c27eb3`. This is library composition for the
centrally owned parser/runtime, not another CLI executable or live-service proof.

## Mounting contract

Use `fabric_cli_resources::frontdoor::register(root)` or compose the four
`acl_subcommand`, `label_subcommand`, `find_subcommand`, `api_subcommand` builders
once. Pass root matches to `AccessCommand::from_matches`; `Ok(None)` means an
unrelated family. Central dispatch carries that DTO in its `Invocation` and calls
`AccessHost::execute` on its existing runtime. The public `access_frontdoor`
example demonstrates the same contract without depending on central declarations.

| Leaf | Typed input / execution |
| --- | --- |
| `acl ls`, `acl dir` | Path tokens, scalar query, long flag -> existing context support check, access SDK and source projection |
| `acl get` | Path tokens, joined query -> existing collection query/path discovery |
| `label list-local` | Only the explicitly configured bounded definitions file; no target or credential lookup |
| `find` | Search text, optional scalar params, joined query, long -> existing catalog session/pager and source projection |
| `api` | Five source verbs, input/params/headers/query and show_headers -> existing bound request engine |

`ApiClients` borrows caller-bound Fabric, OneLake, ARM and Power BI sessions.
The source `-A` spellings remain `fabric`, `storage`, `azure`, `powerbi`; absent
means Fabric, never audience inference from a URL. Power BI intentionally uses
the source Fabric scope with the Power BI origin. No session creates credentials
or falls back to another audience. Existing safe-header, origin, no-redirect,
GET-retry-only, mutation uncertainty and response bounds remain authoritative.
Raw API does not acquire paging or polling behavior.

`AccessTargets` adapts the existing central async context/session resolver.
The pure existing `Resolver` also implements it for caller-owned snapshots.
Only existing workspace/item ACL support is claimed here; resource exclusions
are checked before the service request. Hidden/OneLake resolution is not invented.
ACL GET export is recognized but explicitly rejected before target/service I/O
until an export host is bound. Hidden `--skill` is retained but rejected before
I/O until a shared header binding exists, never silently ignored.

`frontdoor::render` selects the typed surface and optional output override, then
uses the existing family JSON/text renderer. Warnings are returned for the caller
logger. Ordered stdout/stderr records, source exit codes and partial rendering
errors remain explicit; `AccessOutput::write_to` propagates writer failure.
No terminal, account, home config, global logger or runtime is discovered.

## Source and native evidence

New parse-to-real-SDK/shared-HTTP/render consumers read the existing immutable
`access/render_cases.json` and `catalog/cases.json`, rather than creating expected
outputs from native results. They cover all five mounted leaves, text/JSON,
queries, method/URL/body/scope, missing capability, malformed collection,
cancellation and writer failure. Four explicit raw audience bindings are tested.

Before native results, the validator runs the unchanged Python references:
`tests/compatibility/access/reference.py`, `label_reference.py`,
`mutations_reference.py`, `render_reference.py`,
`tests/compatibility/catalog/reference.py` and
`tests/compatibility/raw_api/reference.py`, from `rust/`.
This worker has no Python on PATH and no heavy-build grant; actual native results
belong to the coordinator-routed Linux receipt, not this document.

Run the narrow consumer with
`cargo test --offline --locked -p fabric-cli-resources --test access_contract frontdoor_contract`,
then targeted all-target Clippy and the `access_frontdoor` example.
The only new dependency edge is the already locked workspace Clap library.
No dependency version, feature pin or third-party package is added.

Happy-path token cardinality, aliases, last-option-wins and list/scalar quote
processing are source-backed. The existing read-only lexical adapter is not
reimplemented here. Exact argparse diagnostics, unusual attached/negative-number
spellings and variadic-position resumption are explicit preview lexical debt,
not byte-parity claims. REPL/batch, custom group help and root-only output-option
default placement remain central integration work. Executable mounting and
production host binding are required before these leaves count as working CLI
paths; library tests alone do not add to that numerator.
