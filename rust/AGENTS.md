<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal Rust API contributor instructions

This independent `fab-rs` slice supports only raw API and argument-free
`auth login`, `auth status`, and `auth logout`. Python `fab` remains unchanged.
Reusable libraries belong under `engine/`; executable/presentation code belongs
under `clients/cli/`. Shared HTTP contracts belong to `fabric-core`, never a
resource service. Do not retain SDK facades, resource families, paging, brokers,
REPL, updater, or credential fallback providers.

The CLI explicitly constructs `azure_identity::AzureCliCredential` for the
current Azure CLI context. Azure CLI is the sole supported authentication
prerequisite for this product. Never mutate its login, logout, account, tenant,
or subscription state. Libraries receive injected credentials/HTTP/time and do
not discover ambient identities, initialize runtimes, prompt, or exit.
Persist only tenant/principal binding metadata, never tokens.

Keep dependencies centralized in the explicit workspace manifest, public APIs
documented, and unsafe code forbidden. New test crates deny
`unnameable_test_items`. All behavioral tests are offline and secret-free,
including credential-process tests. Do not run live Azure/Fabric authentication.
Use pinned Python behavior as an independent oracle; document approved
differences rather than changing expectations to match the port.

On this shared host, obtain an explicit integration-owner heavy-build lease
before Cargo check, test, clippy, doc, or build; run only one heavy job at a time.
Formatting, metadata, source inspection, and editing may proceed without a
lease. A quiet host is not a grant. Run pinned commands from `rust/` as listed in
the README. Combined executable evidence, fresh independent architecture/auth/core
and product reviews, and applicable native checks are required; source inspection
or formatting alone does not establish correctness.

Work only in this isolated branch and assigned file area. Do not access the main
checkout or edit peer worktrees. No push, pull request, shared-branch merge, or
release publication is authorized for this implementation. Authentication/core
changes still require Fabric CLI team review before upstream acceptance.
