<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Catalog commands

`fabric-cli-catalog` owns FIND grammar and typed conversion, bounded catalog
search and projection, interactive paging, hidden-resource discovery and the
existing grouped Domain sync adapter. Compose `parser::register` and
`parser::from_matches`, then `execute` using the caller-selected Fabric session.
The caller retains common skill-header policy and output selection.

Interactive output and confirmation remain injected. Domain sync still
requires exact typed resolution and immediate operation-specific approval; this
extraction mounts no new command or provider and does not infer consent.

All implementation comes from v2 checkpoint
`61c8221914152e231e5e64f7bd38418f66264999`; v1 commit
`f8cb97e92eab669785954335e0e7276d30a3377e` is capability-layout prior art only.
`fabric-cli-resources` reexports the complete existing API, including `service`.
Pinned independent oracles and their existing consumers remain in place.
