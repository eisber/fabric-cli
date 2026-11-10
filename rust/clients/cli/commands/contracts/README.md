<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared command contracts

`fabric-cli-command-contracts` is a neutral CLI-layer owner, not a capability
facade. Access, catalog, raw API and the compatibility aggregate depend on these
exact definitions so their errors, values, callbacks and presentation compose
without conversions or duplicate trait/type identities.

## Belongs here

- `CommandError`, its SDK error conversion and `with_context`, `CommandValue`,
  `write_error` and `python_truthy`: source diagnostic/warning retention,
  handled-error/Unauthorized exits and JSON truth-value routing.
- `Confirmation`: the one caller-owned async confirmation trait. Failed or
  cancelled prompts never mean consent.
- `FamilySurface`, `FamilyPresentation`, `render_family_output` and
  `render_family_error`: shared output policy, source surface routing, partial
  output and ordering over the existing `fabric-cli-output` renderer.
- `ResourceHttpExecutor`: the existing adapter over `fabric-sdk-http`, with no
  new HTTP, paging, retry or credential implementation.

## Does not belong here

ACL/label algorithms, search/discovery, raw request construction, command parsing,
host dispatch and process-global setup belong elsewhere. Surface variants
describe existing output policy; they do not execute capability algorithms.
This package depends on no capability or compatibility package.

Callers supply streams, logger observations, presentation, Unicode widths, HTTP
execution and runtime. Output-write failure never causes request replay.
Operation errors preserve possible mutation side effects; cancellation or a lost
response is not rollback. No credential, runtime, browser, terminal or global
configuration is discovered.

## Provenance and verification

These are the existing `fabric-cli-resources` definitions, moved rather than
wrapped or duplicated. The Python baseline remains
`microsoft/fabric-cli@cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`;
the independent access/catalog/raw API fixtures remain in
`rust/tests/compatibility/`. Original public-path tests and examples, including
rendering and lifecycle contracts, stay in
`../../compatibility/resource-commands`. Its extraction-identity test checks
shared types and traits across the capability and legacy paths.
