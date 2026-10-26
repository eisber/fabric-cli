<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent catalog source oracle

`reference.py` loads exact Git object
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, then executes the actual `find`
handler, source parameter/type/query/projection functions and API route helpers.
Only API, UI, logging and interactive-input boundaries are synthetic.

`cases.json` contains 16 command-line scenarios and eight actual hidden read
routes. `interactive_cases.json` adds six independent interactive, repeated-param,
unsupported-param and raw authorization-code cases without modifying the first
capture. Initial capture modes refuse to overwrite existing fixtures; normal
execution only compares.

The Rust consumers exercise SDK -> shared HTTP and CLI -> query/output, including
actual continuation bodies, source-specific collection fields, prompts/EOF and
diagnostics. Source text-table formatting, binary registration, persistence and
live credentials are not established by these fixtures. Run the Python reference
and `cargo test --offline --locked -p fabric-cli-resources --test access_contract`.

`metadata_cases.json` captures six exact singleton helper routes.
`bindings_cases.json` executes 16 whole source GET/enrichment/query/error cases
and 15 case-sensitive full-name/first-match resolutions. Source response merging
uses the original `ApiResponse.append_text` and recursive continuation helper.
The actual SDK/CLI consumer compares request order, untouched fields, projection,
warning and suppressed-error outcomes; extra tests enforce cancellation, original
response byte bounds, gatewayId query persistence and partial-page failures.

`query_dispatch_reference.py` and additive `query_dispatch_cases.json` execute
the actual pinned `do_request`, recursive successful-response helper and
`ApiResponse.append_text`, with synthetic credential/context/HTTP boundaries.
Fourteen cases separately retain observed request URLs, last-dispatched queries,
the original dictionary's state and a later source request after keep/reset/URI
overwrite. Page-two credential failure shows why dictionary mutation and dispatch
are not interchangeable. The initial capture refuses overwrite; normal runs only
compare. Native tests use the actual SDK/shared HTTP stack and keep cancellation,
partial errors, limits and default stateless behavior distinct from an explicitly
caller-owned reconstruction test.
