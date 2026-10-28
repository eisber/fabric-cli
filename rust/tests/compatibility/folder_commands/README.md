<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Folder listing contracts

This suite checks folder-aware listing through command resolution, complete
hierarchy construction, sorting, query and output composition.

Synthetic HTTP pages, scoped credentials, fixed configuration/time and private
streams make execution deterministic. Each command receives isolated caches.
Development dependencies are pinned in the compatibility requirements.

## Graph and paging checks

Valid graphs exercise parent assignment, duplicate IDs, reverse-order arrivals
and multi-pass hierarchy assembly. A safety guard rejects cycles, orphans and
invalid parents before unbounded traversal. It does not construct replacement
parents or expected rows.

Later-page errors and incomplete snapshots must not become successful listings.
Native tests distinguish explicit safety failures from supported response
contracts and preserve actual partial progress.

<a id="reviewed-native-only-evidence-corrections"></a>

## Native boundary expectations

`native-boundaries.json` binds individual safety and snapshot cases to reviewed
contracts and fixed input identities. It supplements rather than replaces
`cases.json`. Cancellation-before-discovery requires
`LongRunningOperationCancelled` and zero requests. Malformed item responses and
unsafe continuations retain the existing item-operation failure classifications.
Equal output with different request sequences remains a snapshot difference,
not an exact match.

## Output checks

Assertions retain ordering, paths, query presence, scopes, stream bytes and
exits. Handler-list query input and raw scalar argv are different boundaries;
use the query-argument suite for parser-level observations.

The harness denies live I/O, ambient identity, process creation and sleeping.
It provides no fixture regeneration or waiver mode. Live credentials, host
persistence and interactive terminal behavior require separate validation.
