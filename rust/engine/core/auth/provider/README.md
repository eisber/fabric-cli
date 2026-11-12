<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit Azure CLI provider

`fabric-identity` constructs `azure_identity::AzureCliCredential` directly with
an injected `azure_identity::process::Executor`. Tenant/subscription overrides
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
groups / Windows Job Objects so dropping acquisition kills its owned process
tree rather than only the SDK shell. Provider output and errors are never copied
into public diagnostics. The supported scopes are exactly the three fixed
product scopes; Power BI deliberately shares Fabric's scope.

`tests/azure_cli.rs` injects the actual Azure SDK process boundary, synthetic
tokens, clock, and cancellation. It does not invoke Azure CLI or contact Azure.
Restricted auth/core changes still require Fabric CLI team review; authored
tests are not evidence of execution until the integration owner runs them.
