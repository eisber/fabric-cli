<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Raw API command

`fabric-cli-api` owns the existing raw API schema, typed conversion, bounded
JSON/file/multipart preparation and execution over explicitly supplied SDK
sessions. Compose `parser::register`, `parser::from_matches` and
`ApiClients::execute`. Registration and parsing perform no file or credential
access. The caller retains common skill-header policy and output selection.

Audience selection still precedes input-file loading. The Fabric, storage,
ARM and source-compatible Power BI bindings are explicit; no missing binding is
substituted. Existing verbs, allocation bounds, request validation, raw HTTP
status/body behavior, query projection and cancellation are unchanged.

This extracts v2 checkpoint `61c8221914152e231e5e64f7bd38418f66264999`,
using v1 `f8cb97e92eab669785954335e0e7276d30a3377e` as layout prior art, not
as a replacement baseline. Existing `fabric-cli-resources` imports remain
reexports. Credentials, transport and runtime selection stay with the caller.
