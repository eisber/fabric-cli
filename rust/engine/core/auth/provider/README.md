<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit Azure CLI provider

`fabric-identity` constructs `azure_identity::AzureCliCredential` directly with
an injected `azure_identity::Executor`. Tenant/subscription overrides
remain unset. There are no fallback credentials, native login, account commands,
SDK facades, filesystem access, global runtime, or token persistence.

`AzureCliSession::login(context)` validates the active SDK-provided token's
`tid`/`oid` metadata and expiry. `Binding` contains only normalized tenant and
principal object IDs; token claims are consistency metadata, not local
cryptographic proof. The host persists selection.

`BoundCredential::new(session, binding)` implements the Azure SDK
`azure_core::credentials::TokenCredential`, accepting exactly one supported scope
and forwarding SDK request options without overrides. It owns per-scope mutex slots for single-flight
in-memory acquisition, reuses tokens with more than 30 seconds remaining, and
checks the binding on every fresh acquisition, including polling refresh.
Detected drift or invalid identity metadata disables every cached scope for
that bound credential, including still-unexpired tokens.
Subscription changes alone are not drift. The Azure SDK provider itself has
no token cache.

The HTTP executor and auth-status host bound SDK acquisition, including mutex
waits, with their operation cancellation/deadline; explicit login is bounded
internally. Other library callers must likewise bound and drop SDK acquisition
on cancellation/deadline. The
production executor bounds stdout/stderr and uses `process-wrap` Unix process
groups / Windows Job Objects so dropping acquisition requests termination of
the SDK shell and contained descendants. It does not await a cleanup-completion
barrier; a Unix descendant that leaves the group/session is not contained by
this mechanism. Provider output and errors are never copied
into public diagnostics. The supported scopes are exactly the three fixed
product scopes; Power BI deliberately shares Fabric's scope.

`tests/azure_cli.rs` injects the actual Azure SDK process boundary, synthetic
tokens, clock, and cancellation. It does not invoke Azure CLI or contact Azure.
`tests/process_tree.rs` exercises the production executor and real SDK shell in
an isolated subprocess with a native fake `az` and contained descendant. It
checks cancellation, real deadlines, inherited-pipe stalls, bounded return and
OS process termination. Its private PATH/configuration never changes the test
runner's environment, and no installed Azure CLI or Azure endpoint is used.
Restricted auth/core changes still require Fabric CLI team review; authored
tests are not evidence of execution until the integration owner runs them.
