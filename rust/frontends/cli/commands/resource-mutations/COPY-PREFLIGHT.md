<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# OneLake copy preflight

Two pure CLI-layer functions expose the **existing**, unchanged copy policy:

```rust,ignore
fabric_cli_mutations::validate_copy_destination(&target)?;
// Existing filesystem host: is_directory (reject), then is_file (require).
let file = fabric_cli_mutations::file_destination(&target, &source_name)?;
// Existing filesystem host: progress, read actual local bytes, reject empty,
// then the existing upload_bytes helper and real shared SDK execution.
```

`validate_copy_destination` wraps the existing `writable(target, false)`.
`file_destination` wraps the existing `destination` and never calls the root
validator again. Both return `OneLakeWriteError`; neither requires an SDK
client, credentials, executor, UI, local filesystem, runtime, or empty upload.
The order is contractual: a rejected destination wins over missing/recursive
local source; filename inference occurs only after the file check, before read.
Do not replace actual source I/O with these helpers or fake a zero-byte upload.
`upload_bytes` and `copy_file` retain their existing behavior and checks.

`examples/offline_copy_preflight.rs` is a public synthetic caller, not production
filesystem orchestration. Its observable collaborator trace is compared with
the pinned original `copy_local_2_onelake`, `check_onelake_destination` and
`get_onelake_file_destination` bodies by `tests/copy_preflight_reference.py`.
The separately owned filesystem host still owns local failures and dispatch.
