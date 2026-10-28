<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Catalog and hidden metadata contracts

This suite covers catalog search, interactive continuation, hidden collections,
singleton metadata, name resolution and dispatched-query observations.

Fixed fixtures check ordered routes, request bodies, collection fields,
projection, prompts, EOF, warnings and error precedence. Metadata cases retain
untouched fields, optional omissions and partial failures.

Reference execution uses injected API, UI, logging and interactive-input
boundaries without authenticating or calling a service. Normal runs compare
immutable expectations; initial evidence capture cannot overwrite an existing
fixture.

`query_dispatch_reference.py` checks the last request actually dispatched,
dictionary state and subsequent query lifetime independently of the next
continuation token. Native consumers exercise the real SDK/shared-HTTP stack
with explicit cancellation, byte bounds and default stateless behavior.

These tests do not establish terminal rendering, persistence or live
authorization. Actual command and host integration require their own checks.
