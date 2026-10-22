<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Identity backend decision

**Status: unresolved.** Production interactive authentication is blocked;
workspace scaffolding and direct SDK credential injection are not.

## Accepted boundary

SDK consumers inject an
`Arc<dyn azure_core::credentials::TokenCredential>`. They do not need
`fabric-identity` or the CLI. Account, tenant, and audience selection are explicit;
the caller must bind the injected provider to that selection. Selection metadata
does not independently verify token claims.

The scaffold has no production credential provider. The example's in-memory,
expired dummy token is test-only and cannot establish a real session.
`interactive_credential` returns `IdentityError::Unsupported` without launching
a browser, probing ambient credentials, invoking tools, or persisting data.

The shared error vocabulary distinguishes unsupported capability, missing
configuration, interaction required, cancellation, and refresh failure.
Future adapters must preserve these distinctions, sanitize diagnostics, and
never silently fall back to another account, tenant, audience, or backend.
There is no current adapter translating Azure provider errors into these
variants; returning one generic refresh error for all failures would be wrong.

## Why no backend is selected

The assignment's baseline research has not established an official Microsoft
Rust MSAL SDK or a supported generic cross-platform MSAL C ABI. Community crates
named `msal` or `msal-rs` must not be treated as official Microsoft bindings.

Browser MFA can work without MSAL, but that alone does not demonstrate OS-account
SSO, device-bound tokens, broker capabilities, or satisfaction of tenant
Conditional Access requirements. Generic OAuth must not silently substitute for
those enterprise requirements.

Copilot Rust's broker-aware flow delegates to its runtime and derives GitHub EMU
credentials. It is not a verified general Fabric, OneLake, or ARM credential
provider. Its Azure local-development example uses `DeveloperToolsCredential`,
which invokes `az`/`azd` and violates this product's installation requirement.
Neither Copilot's SDK nor its runtime is included here.

## Required proof before choosing an implementation

1. Establish supported interfaces and official support ownership per platform.
2. Demonstrate explicit account, tenant, and audience handling, including the
   relevant Fabric, OneLake, and ARM flows.
3. Validate enterprise SSO, MFA, device-bound and Conditional Access requirements
   against representative tenant policies. Document unsupported combinations.
4. Prove cancellation, refresh, interaction-required and error classification
   without fallback or credential disclosure.
5. Establish secure platform persistence, cache isolation, and logout behavior.
   No plaintext persistence or successful-looking stub login is acceptable.
6. Resolve enterprise proxies/trust, certificate authentication prerequisites,
   helper packaging, redistribution, signing, and notarization.
7. Verify authentication without installed Azure CLI, Azure Developer CLI, or
   Python.

A packaged official-platform helper remains an option, subject to supported
interfaces, redistribution rights, native packaging, and tenant-policy proof.
No helper or generic OAuth implementation is selected by this scaffold.

Explicit unattended Azure identity adapters can be considered independently,
adding `azure_identity` only when exercised. They must not use a developer-tool
fallback chain, expose credentials in arguments/logs/snapshots, or silently
persist credentials. Authentication implementation requires Fabric CLI team
review.
