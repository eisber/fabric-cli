<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric identity adapters

`fabric-identity` is an optional identity library, separate from the SDK.
SDK consumers continue to inject `Arc<dyn TokenCredential>` directly. The
default feature set is empty and performs no credential discovery.

## Headless authentication

The explicit `headless` feature composes official `azure_identity` 1.0.0
credentials with a caller-provided `azure_core::http::HttpClient`. Azure's
default HTTP client and developer credential chains are not enabled.
Construction does not create a runtime, acquire a token or contact a provider.

`HeadlessCredential::new` accepts one `HeadlessInput`:

| Input | Official provider | Contract |
| --- | --- | --- |
| `ClientSecret` | `ClientSecretCredential` | Explicit GUID tenant/client and caller-supplied `Secret`; secret is an immutable snapshot |
| `AssertionFile` | `ClientAssertionCredential` | Explicit GUID tenant/client and renewable, bounded local assertion file; suitable for configured AKS workload federation |
| `AzurePipelines` | `AzurePipelinesCredential` | Explicit GUID tenant/client/service connection, running-job OIDC endpoint and caller-supplied secret system token; a new OIDC assertion is fetched on each access-token refresh |
| `ManagedIdentity` | `ManagedIdentityCredential` | Explicit Azure VM or App Service host and explicit system-assigned or GUID client selector |
| `Certificate` | `ClientAssertionCredential` | Separate `certificate-store-windows` feature; explicit non-exporting signer, public-cloud tenant/client and immutable certificate fingerprint |

Use the caller's Tokio runtime for token acquisition. The wrapper accepts exactly
one Fabric, OneLake, ARM or native SQL public-cloud `.default` scope. It rejects unknown,
empty and combined scopes before any provider or file work. OAuth requests are
pinned to the selected tenant at `login.microsoftonline.com`; ambient authority
overrides are not used. Endpoint bindings for Fabric daily/MSIT service traffic
do not change the token authority or audience.

Native SQL requests use `https://database.windows.net/.default`, never a Fabric
REST token. SQL is acquired only on an explicit credential request. Existing
Windows login and status retain their original Fabric/OneLake/ARM sequence;
an explicit SQL refresh uses the same bound account and remains silent.
Supporting this scope does not grant SQL database permissions, enable a query
binding, or establish live Fabric SQL authentication support.

The official providers maintain memory-only access-token caches, separated by
scope, and renew within five minutes of expiry. Assertion files are read again
when the official provider needs a new access token, not on every cached-token
call. There is no separate ten-minute assertion cache. Reads run off the async
worker, accept at most 64 KiB of UTF-8 ASCII-graphic assertion text with optional
trailing line endings, and require a regular file. Paths must be absolute and
local, without parent traversal or Windows device/network prefixes. Construction
pins validated directory handles without opening the assertion. Parent symlinks
are rejected, except the explicitly bounded Unix `/var/run` → `/run` system
alias; use physical paths for other redirected parents. Refresh never
recanonicalizes the caller's parent path.

On Unix, descriptor-relative no-follow/nonblocking opens validate the actual
handle as a regular file before reading, so FIFOs cannot retain a blocking
reader. Projected AKS relative token and `..data` links may rotate beneath the
pinned root, with bounded link traversal. Absolute leaf links and attempts to
traverse above that root fail before an outside target is opened. Replacing the
original parent path cannot select a different root for an existing provider.

On Windows, directory handles deny write/delete sharing while the provider is
alive. Every parent and the leaf are opened without following reparse points;
reparse points and DOS device/alternate-stream names are rejected. Only the
drive root is classified once, with network/device canonical targets rejected
before token-file opening. Windows reparse-based assertion rotation is not
supported; regular-file replacement between reads remains supported.
No token claims are decoded or treated as identity proof.

### Azure Pipelines workload federation

Select `HeadlessInput::AzurePipelines` with a `ServiceIdentity` and
`Box<PipelinesConfig>`. Capture configuration with
`PipelinesConfig::from_inputs(service_connection, oidc_uri, system_token, read)`;
`read` is a caller-provided environment lookup closure. Construction captures
only the following non-secret run inputs:

| Input | Required binding |
| --- | --- |
| `SYSTEM_COLLECTIONURI` | Canonical `https://dev.azure.com/{organization}/` collection |
| `SYSTEM_TEAMPROJECTID` | Project GUID |
| `SYSTEM_HOSTTYPE` | Exact `build`, `rm` or `checks` hub |
| `SYSTEM_PLANID` | Plan GUID |
| `SYSTEM_JOBID` | Job GUID |
| `SYSTEM_OIDCREQUESTURI` | Exact endpoint derived from the collection, project, hub, plan and job |

Supply the service connection GUID, explicit OIDC URI and `Secret` system token
separately. The token must contain 1–4096 ASCII graphic bytes. Neither token
presence nor a workload assertion file selects this provider. The service
connection must already have workload federation configured for the selected
tenant and client; this library never creates registrations or federation policy.

The accepted URI is exactly
`{collection}{project}/_apis/distributedtask/hubs/{hub}/plans/{plan}/jobs/{job}/oidctoken`,
without query, fragment, credentials, encoded path substitutions or alternate
ports. Legacy `visualstudio.com`, sovereign and self-hosted DevOps endpoints
are outside this bounded contract. The official SDK appends exactly
`api-version=7.1` and the selected `serviceConnectionId`; it sends a bodyless
POST with the secret bearer system token and `x-tfs-fedauthredirect: Suppress`.
That header/token cannot accompany the Entra token request.

Official `azure_identity` **1.0.0** has no public OIDC endpoint injection option:
its constructor reads `SYSTEM_OIDCREQUESTURI` from the process environment.
The adapter does not replace that provider, rewrite its URL, or mutate the
environment. Instead, it checks that the process's six run inputs exactly
match the caller's capture before and after official-provider construction,
on every token call (including cache hits), immediately before each HTTP send
and after acquisition. A changed or missing source fails closed. Selection
metadata is a caller contract, not independent proof of the pipeline's identity.

The HTTP client is caller-injected and **must disable redirects and automatic
retries**, as the CLI's native transport does. Both OIDC and Entra requests are
pinned to their exact selected endpoints. The wrapper rejects redirects without
following `Location`, strips response headers before SDK processing, bounds OIDC
JSON to 64 KiB, and discards provider error bodies and source chains. It never
logs a system token, assertion or access token. Dropping acquisition futures
stops local waiting without spawning background refresh tasks; a server-side
request already sent cannot be rolled back.

The official access-token cache is single-audience and memory-only. Every cache
miss/refresh obtains a fresh assertion from the selected pipeline endpoint; the
adapter does not persist or separately cache assertions. The system token is an
immutable input snapshot: replacing it requires constructing a new credential.
`PipelinesConfig::binding()` distinguishes provider, connection and run for batch
isolation without incorporating the system token or decoding access-token claims.

The offline suite uses the **actual official provider**, not a substitute
credential. Since its public API requires process environment, tests launch an
isolated child test process with synthetic run inputs and injected fake HTTP.
Parent tests and pure capture tests never mutate global environment. These
tests are source-ready; validation of this addition is pending a numbered
controller build grant, independent identity review and native execution.

No dependency or feature additions are needed: `AzurePipelinesCredential` is
exported unconditionally by the existing `azure_identity = "=1.0.0"` dependency
with defaults disabled. The current lock resolves `azure_core` to 1.1.0.
No shared manifest or lockfile change belongs to this addition.

After a **new numbered controller grant** with fixed expiry and exact allowed
commands, the controller should run the following plan from `rust/`, holding its
named build mutex with CPU 2/build jobs 2 and Rust test threads 1:

1. Check both changed packages with `--locked --offline --all-targets
   --features headless`, then run Clippy for that same scope with `-D warnings`.
2. Run identity unit tests filtered by `headless::pipelines` and the `headless`
   integration target filtered by `pipelines::`; this includes the isolated
   child harness exercising the real official provider.
3. Run CLI binary tests filtered by `headless::tests`, and the
   `headless_configuration` process target. Exercise the existing native batch
   binding tests without changing their expectations.
4. Run the existing secret/assertion/managed-identity regressions and affected
   full-source/architecture contracts through the controller's existing plan;
   preserve all source pins and expectations.
5. Obtain native Windows, Linux x64 and Apple ARM execution as applicable.
   Cross-target check/Clippy on Windows is not Unix runtime evidence.

The focused command candidates for that grant are:

```text
cargo check --locked --offline --jobs 2 -p fabric-identity -p fabric-cli --all-targets --features headless
cargo clippy --locked --offline --jobs 2 -p fabric-identity -p fabric-cli --all-targets --features headless -- -D warnings
cargo test --locked --offline --jobs 2 -p fabric-identity --features headless --lib headless::pipelines -- --test-threads=1
cargo test --locked --offline --jobs 2 -p fabric-identity --features headless --test headless pipelines:: -- --test-threads=1
cargo test --locked --offline --jobs 2 -p fabric-cli --features headless --bin fab-rs headless::tests -- --test-threads=1
cargo test --locked --offline --jobs 2 -p fabric-cli --features headless --test headless_configuration -- --test-threads=1
```

No Cargo, test or Clippy command was executed for this addition without that
grant. Formatting/source inspection cannot certify compilation or test behavior.
No live acquisition, tenant policy, registration, signing or secret operation
is part of this plan.

### Managed identity

For managed identity, the host contract must already match the environment used
by the official provider. VM selection requires all alternate managed-host
selectors to be absent. App Service requires the exact selected
`IDENTITY_ENDPOINT` and the explicitly selected `IDENTITY_HEADER` environment
source, with other host selectors absent.
The endpoint must be a literal loopback or link-local HTTP address without user information,
query or fragment. The contract is revalidated at construction, before every
token call (including cache hits), and before sending an HTTP request. The
platform header may rotate: the actual outgoing header must match a fresh read
of `IDENTITY_HEADER` for every metadata request. A rotation race fails closed
without implicit retry. Header values are not principal or session-binding
fingerprints. No environment is mutated and no alternate host is attempted.
The CLI injects a separate, exact-endpoint, GET-only metadata HTTP client; its
service client remains HTTPS-only. Other App Service local-address classes are
unsupported pending review; this is not universal App Service acceptance.

Provider errors are replaced with bounded typed diagnostics. The wrapper's
`Display`, `Debug` and error source chain do not retain OAuth responses, HTTP
bodies, paths, assertions, access tokens or client secrets. Credentials are not
written to disk. Non-success provider bodies are discarded before SDK error
construction; successful JSON bodies are limited to 256 KiB and expiry metadata
is checked before the official parser. Rejected credentials, unavailable
providers, invalid metadata and unsafe assertion files retain distinct sanitized
failures. Dropping a token future stops local waiting; it does not imply
provider rollback or cancellation of a blocking filesystem operation.

### Executable configuration

The CLI's separate `headless` feature supplies explicit environment selection and
the native service-command host. See
[native host configuration](../../../docs/native-read-host.md#headless-credentials).
No interactive `auth login` is required for headless service commands.

### Support limits

These are experimental adapters with offline official-provider and native-command
contracts, not live tenant or production acceptance. Fabric CLI identity/core
review and live Windows, macOS and Linux acceptance remain required before GA.
Service permissions, workload federation setup, managed-host assignments, tenant
policy and trust/proxy acceptance must be validated in the intended environment.
Managed tenant labels are not token-claim verification.

Windows certificate-store authentication is described below. macOS and Linux
native-store adapters remain unavailable; this does not waive cross-platform GA
requirements.
There is no generic CI credential claim, raw Fabric access-token input, Python,
Azure CLI, Azure Developer CLI, browser fallback, plaintext token cache or
automatic account selection.

### Windows certificate-store authentication

The optional `certificate-store-windows` feature provides an experimental
Windows-only certificate adapter. This source is not yet independently reviewed,
compiled or executed. It is not a production or GA support claim.

Version 1 accepts only an existing certificate in the explicitly selected
`CurrentUser\MY` or `LocalMachine\MY` store, identified by its exact SHA-256 DER
fingerprint. The certificate must already have an associated RSA signing key
whose **actual acquired provider** is the **Microsoft Software Key Storage
Provider** and whose readable export policy forbids export and archival.
Provider policy is a post-acquisition allowlist, not a request to choose a KSP.
The adapter never creates, imports, exports, repairs or rebinds a private key.
Non-exportability is provider policy, **not hardware attestation or FIPS
certification**. TPM, smart-card and third-party providers are not supported in
this version and are not silently substituted.

SDK consumers remain independent and may inject any appropriately bound
`TokenCredential`. Identity-library callers can supply a `CertificateSigner`
through `CertificateConfig`; `WindowsStoreSigner` supplies the isolated native
implementation. Constructors capture metadata only. Key access occurs only
when the official `ClientAssertionCredential` needs a new assertion.

This adapter supports **Azure public cloud only**. Tenant/client GUIDs are
explicit. The JWT `aud` and guarded OAuth request must both equal
`https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token`; ambient authority
settings cannot replace either. No sovereign-cloud or custom-authority selector
is accepted. Cryptography uses BCrypt/NCrypt: PS256 with SHA-256,
MGF1-SHA-256 and a 32-byte salt. `x5t#S256` is the unpadded base64url SHA-256
fingerprint of the selected certificate DER. There is no `x5c`, SHA-1 identifier,
OpenSSL dependency, PKCS#12 provider, certificate-file/password input or private
key serialization.

Assertions use a fresh 128-bit system-random identifier and a five-minute
lifetime. `iat` and `nbf` use the current UTC Unix second without backdating.
Invalid/overflowing clock values, a backwards clock observed after signing, or
an assertion that expires while signing fail closed. Native signing retains
the same matched certificate context through acquisition, provider/key-policy
checks, signing and public-key verification.

Certificate work has its own **two-worker** capacity, independent of WAM.
Saturation produces a typed failure rather than an unbounded queue or another
worker. Silent acquisition and signing never permit UI. Prompt-required keys
fail explicitly. Cancellation invalidates late publication but cannot interrupt
a synchronous CNG call; its certificate-only capacity remains occupied until
actual teardown. Unconfirmed cleanup quarantines that capacity.

The official provider caches access tokens in memory per audience. Every
assertion refresh re-resolves the same explicit certificate selection; it does
not permanently retain an old key handle. A different certificate requires an
explicit new fingerprint, credential and batch binding. Missing/revoked keys
never trigger a different certificate, store, provider or cached-token fallback
when renewal is required.

For the feature-enabled CLI, select `FAB_RUST_AUTH_MODE=certificate-store-windows`
with the existing explicit tenant/client selectors and:

| Environment input | Required value |
| --- | --- |
| `FAB_RUST_CERTIFICATE_STORE` | `current-user-my` or `local-machine-my` |
| `FAB_RUST_CERTIFICATE_SHA256` | Exactly 64 hexadecimal fingerprint characters |
| `FAB_RUST_CERTIFICATE_PROVIDER` | Exact `microsoft-software` allowlist policy |

No certificate operation occurs for help/version, invalid selection, an
unselected provider, or merely linking the library. Operating-system key ACLs,
registration/service permissions, software-KSP behavior and clean-machine native
packaging still require independent authentication/native/team review and
explicit acceptance. Native macOS/Linux backends and broader Windows KSP support
remain separate, unproven requirements.

Sources for the Pipelines contract: released
[`AzurePipelinesCredential` 1.0.0](https://docs.rs/azure_identity/1.0.0/src/azure_identity/azure_pipelines_credential.rs.html)
and Microsoft's
[OIDC token endpoint](https://learn.microsoft.com/en-us/rest/api/azure/devops/distributedtask/oidctoken/create?view=azure-devops-rest-7.1).
The REST reference labels its API `7.1-preview.1`; this adapter deliberately
preserves the official Rust 1.0.0 provider's `7.1` query rather than rewriting it.

## Windows broker

`windows-broker` enables the separate experimental Windows credential adapter.
Only explicit login permits broker UI; SDK refresh is silent and identity-bound.
Headless authentication does not replace or relax that interactive contract.
