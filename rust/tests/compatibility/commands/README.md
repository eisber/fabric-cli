<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Read command contracts and inventory

This directory defines command inventory and offline contracts for workspace
and item listing and metadata GET presentation.

`cases.json` contains fixed synthetic request, output and exit expectations.
`native-boundaries.json` separately records safety, unsupported-domain and
schema-order assertions. Native observations never redefine these fixtures.

## Execution

The reference composes real command, context, request, query and output behavior
with injected HTTP, credentials, configuration, current context and time.
Requests must consume their scripted responses and acquire the expected scope.
Unplanned requests fail.

The guarded execution boundary denies application file reads/writes, network,
process creation, sleeping and environment mutation. Source/fixture acquisition
and explicitly selected native probe execution occur outside that boundary.

## Assertions

Listing cases cover ordering, hidden names, future-type filtering, long IDs,
pagination and authorization errors. GET cases check path inventories,
projection and scalar presentation. Full GET enrichment, definition export and
confirmation require the corresponding command-level suites.

Inventory tests validate canonical leaves, aliases, arguments and registrations.
Registration counts do not prove execution support. Preserve absent versus
empty query semantics and command-specific default columns.

Use the workspace validation runner for build/probe commands and retain actual
streams, requests and exits as separate artifacts. No fixture regeneration,
automatic acceptance or waiver mode is provided.
