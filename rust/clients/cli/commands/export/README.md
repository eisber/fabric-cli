<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Item-definition export

`fabric-cli-export` owns the existing `export` and `bulk-export` root schemas
(`src/parser.rs`). `src/host.rs` implements single-item and selected/all workspace
export; `src/bulk_export.rs` implements recursive workspace/folder bulk-export.
`src/files.rs` captures validated export plans and performs ordered asynchronous
writes with original partial-path and cancellation evidence.

Call `execute` with a caller-owned `DefinitionCommandContext`, parsed command and
cancellation future. Shared interfaces and local file-policy adapters are in
[`contracts/import-export`](../../contracts/import-export/README.md).
The command preserves sensitivity warnings, prompts, snapshot boundaries, formats,
local/Lakehouse output rules, response-index summaries and completed IDs.
This is item-definition export, not table-data export or arbitrary file copying.
The parsed legacy deployment leaf remains explicitly unsupported here.

The [definition contract](../../../../authoring-docs/definition-transfer-compatibility.md)
and unchanged source oracle remain authoritative. The existing
`fabric-cli-transfer` API delegates here through its
[compatibility adapter](../../compatibility/import-export/README.md).
