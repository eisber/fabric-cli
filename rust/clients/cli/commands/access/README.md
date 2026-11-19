<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access commands

`fabric-cli-access` owns ACL and sensitivity-label grammar, typed argument
conversion, local label definitions, reads, mutations and ACL export composition.
Use `parser::register` and `parser::from_matches`, then supply the existing
Fabric API client, command-local resolver and confirmation to `host::AccessHost`.
`host::execute_local` runs label listing without a remote host.

The v2 `grant`/`revoke` aliases, help, scalar versus token-joined queries,
present-empty query behavior and record-valued unqueried text GET are preserved.
JSON, explicit queries and export keep their existing contracts. Missing
confirmation or export capabilities remain explicit failures, not consent or
successful no-ops.

The behavior comes from v2 checkpoint
`61c8221914152e231e5e64f7bd38418f66264999`; v1 commit
`f8cb97e92eab669785954335e0e7276d30a3377e` supplies capability-layout prior art
only. `fabric-cli-resources` retains public reexports and its original host
struct layout. Source oracles remain unchanged in their existing test consumers.
No provider, runtime, terminal or mutable global session is installed here.
