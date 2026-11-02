<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Workspace metadata GET contracts

This suite checks primary workspace metadata and its managed-private-endpoint,
Spark-settings and role-assignment enrichments through query and output handling.

Fixed cases retain ordered requests, Fabric scopes, service fields, omission
rules, exact streams and exit codes. Status and truthiness fixtures cover
additional response branches. Explicit native safety and unsupported-domain
cases remain separate from matching behavior.

The reference verifies recorded source/fixture identities and executes with
scripted HTTP, scoped credentials, fixed configuration/time and private writers.
Extra or missing responses fail. Forbidden I/O invalidates a run even when an
optional-read handler catches the attempted operation.

Native probes receive inputs and scripted responses, never expected values.
They exercise the actual workspace SDK, shared HTTP, query and output path.
Later-page errors, unsafe continuations, malformed primary metadata and
cancellation must not become complete-looking metadata.

Argument registration is covered by the query-argument suite. Definition
export, live authorization, persisted context and interactive terminal behavior
require separate tests.

Use the repository's read validation runner for native probes and reference
comparisons. There is no update, acceptance, fixture regeneration or waiver mode.

## Baseline integrity

[Archived baseline evidence](frozen-baseline-evidence.md) preserves the complete
immutable document separately from this README. The archive and six runner,
test, fixture and dependency artifacts remain content-pinned. This README can
evolve without changing those pins or behavioral expectations.
