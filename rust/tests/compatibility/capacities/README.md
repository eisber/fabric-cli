<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Capacity listing contracts

This suite verifies Fabric capacity listing with hand-authored synthetic
responses, routes, query values, ordering and failures.

`cases.json` is immutable. The reference checks recorded source and fixture
identities before executing against injected sessions, credentials, clocks and
logging. It never authenticates, sends HTTP, sleeps or imports application
initialization.

Contracts cover default scope selection, token traversal, duplicate rows,
query encoding, status-specific error precedence and retry behavior.
Native tests separately enforce page/row/byte limits, cancellation, unsafe
continuations and typed partial progress.

A matching reference result does not establish a CLI command, ARM management,
live-service authorization or complete platform behavior.

Run the reference and focused SDK checks through the repository's validation
plan. Keep observed reports separate from fixed expectations; there is no
record, accept or baseline-regeneration mode.
