<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Item-definition import

`fabric-cli-import` owns the existing `import` root schema (`src/parser.rs`) and
command workflow (`src/host.rs`). It imports one item's definition, not arbitrary
files, table rows or a CICD repository. `src/environment.rs` owns the existing
Environment readiness/publish lifecycle; it does not add upload or commit APIs.

Call `execute` with a caller-owned `DefinitionCommandContext`, parsed command and
cancellation future. Shared interfaces and local file-policy adapters are in
[`contracts/import-export`](../../contracts/import-export/README.md).
The command performs the original bounded target resolution, input classification,
confirmation, preparation, create/update, publication and cache-effect ordering.
No credentials, runtime, filesystem roots or UI are discovered.

The [source contract](../../../../authoring-docs/import-command-parity.md) and
unchanged independent oracle remain authoritative. The existing
`fabric-cli-transfer` API delegates here through its
[compatibility adapter](../../compatibility/import-export/README.md).
