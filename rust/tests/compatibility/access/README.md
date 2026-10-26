<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent access-command reference

Source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. `reference.py` executes
the actual ACL handlers, API route functions, LS formatter and JMESPath wrapper
from that Git object with synthetic API/UI boundaries. It never imports the
product, identity providers or Rust. JMESPath 1.0.1 is a development-only oracle
dependency, not a production requirement.

The initial, additive `cases.json` capture contains 39 cases: all five resource
routes, short/long/raw-query LS, raw/path-inventory/scalar GET and empty output.
SHA256: `3d6c147e6aaf35cb98f59983be126e19e9ad2ac657a24ea418c84b903502eeac`.
The capture command refuses to overwrite an existing oracle. Normal execution
only compares. No existing compatibility fixtures are changed.

From `rust`, run `python tests/compatibility/access/reference.py`, then
`cargo test --offline --locked -p fabric-cli-resources --test access_contract`.
`label_local.json` separately records seven source-derived local-file/error
contracts; `label_reference.py` executes the actual handler. `streams.json`
contains 39 exact stdout/stderr/exit captures built using the pinned Python output
classes at fixed time, without reading Rust output.

The Rust consumer exercises real access SDK -> shared API pager -> shared HTTP
executor -> injected Azure HTTP client, plus CLI query/output consumers.

Safety-specific cases are separate Rust tests: malformed payloads, accepted but
incomplete statuses, redirects, origin/path validation, bounds, cancellation,
partial errors and error-code precedence. These are explicit migration safety
behavior, not claims that Python implements those bounds.

`mutation_cases.json` adds 47 independent source-derived ACL/label mutation cases:
all three writable resource routes, role validation/case, source text-substring
update, four-kind principal probing, four identity selectors for delete,
confirmation/overwrite decline, local label lookup/config and payloads.
`mutations_reference.py` executes pinned handler/route functions and refuses to
overwrite an existing capture. Rust compares actual requests, JSON bodies,
success/error messages, diagnostics, warnings and confirmation calls.

Current gaps: frontend path/hidden-resource resolution and parser registration,
central-host use of the family renderer, GET export sinks, live credentials/service calls,
all native platforms and broader front-door completion. Auth/transport/5xx
failures are not swallowed by principal probing; this source exception is
separately tested, not a rewritten Python expectation.

The resumed release now exercises the shared 202 operation policy: additive
`operation_cases.json` preserves source non-GUID behavior, while
`operation_valid_cases.json` separately compares actual valid-ID completion.
The previously nested non-GUID test is registered and executes; it is not waived.
`wire_cases.json` compares actual request and merged ACL bytes, including ASCII
Unicode escapes, spaces, key order and escaped-identity substring differences.
`branch_ledger.json` inventories nine leaves, arguments, source support and whole
handlers/client control flow. It is a denominator, not a claim that every branch
or the public binary is complete.

`render_reference.py`/`render_cases.json` add 64 whole-handler plus original-UI
JSON/text cases at width 48. They retain ordered stdout/stderr writes, not just
the two concatenated streams. Source collaborators are injected at the API/file,
terminal toolkit, timestamp and logging boundaries; actual handler projection,
query, table, success/error and FIND summary logic executes from the Python pin.
Error-only cases inject typed failures into the original UI; they complement,
not replace, the existing root-dispatched OneLake boundary cases.
Native `render_contract` consumers use actual SDK -> HttpExecutor -> family
projection -> released text/JSON renderer -> caller writers. The short-FIND
consumer supplies the source width of generated U+2026 explicitly; another test
continues to require missing-Unicode-property failure. Existing oracle JSON is
not regenerated, including on the first missing-width consumer failure.

`boundaries.json` separately captures five actual GET export intents and four
root-dispatched OneLake error/warning/guidance branches. Storage I/O is outside
that injected boundary; an additional output consumer checks handled error exit 0.
`mutation_edges.json` records seven further cases, including first duplicate
assignment/label matches, empty roles, the SET/list-local missing-key difference,
and actual Python malformed-row exceptions. Rust's typed malformed-row failure
is a labeled safety behavior, not substituted Python output.
