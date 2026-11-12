<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Notebook workload read foundation

**Status: source under development, not platform-qualified or release-ready.**
This opt-in Rust foundation binds public-cloud Power BI (PBI) authentication,
cryptographically verified principal continuity, and explicitly selected
Notebook workload reads. Source availability does not establish a released
capability, successful platform validation, or live service authorization.

The foundation adds no Fabric CLI routes. It does not create or update
notebooks, start compute, control sessions or kernels, open WebSocket
connections, or collect telemetry. It does not select a capacity, workspace,
notebook, cluster, environment, or credential automatically.

## Features and ownership

| Surface | Opt-in API | Responsibility |
| --- | --- | --- |
| `fabric-core/verified-principal` | `principal::EntraJwtVerifier` | Verify signed Fabric and PBI tokens against the selected account and tenant |
| `fabric-sdk-http/notebook-workload` | `notebook_workload::NativeNotebookExecutor` | Construct the real verifier and bind bootstrap and reads to explicit selections |
| `fabric-identity` provider APIs | Explicit PBI credential wrappers | Request the additional fixed scope without changing the selected identity |

Both core and HTTP features are default-off. The HTTP feature enables
`fabric-core/verified-principal`; it does not accept a replacement
decode-only verifier as proof. Direct SDK consumers still inject
`Arc<dyn TokenCredential>` and do not need an identity-provider implementation.

The adapter implements the existing `NativeExecutor` contract. It reuses the
shared `HttpExecutor`, caller-owned HTTP client and timer, and existing
`NotebookRuntime` request builders. It adds no runtime, output engine, HTTP
client, retry loop, or Notebook protocol engine.

## Audience and credential opt-in

`Audience::PowerBiBootstrap` requests exactly
`https://analysis.windows.net/powerbi/api/.default`. This is separate from
`ServiceOrigin::PowerBi`, which retains the Fabric audience and legacy
`/v1.0/myorg` API root. It does not change `NativeFabricExecutor`, its Fabric
origin restrictions, or its existing authentication behavior.

| Provider | Explicit entry point | Selection boundary |
| --- | --- | --- |
| Headless | `HeadlessCredential::new_with_pbi_bootstrap(input, http)` | Preserve the selected provider and its existing configuration restrictions |
| Windows broker | `credential.with_pbi_bootstrap()` on `Arc<WindowsCredential>` | Wrap the same account, tenant, and session generation |
| Interactive | `credential.with_pbi_bootstrap()` on `Arc<InteractiveCredential>` | Wrap the same selected session and retain refresh/logout fencing |

The existing headless constructor does not opt in. The Windows and interactive
wrappers return `Arc<dyn TokenCredential>` over the existing credential instance;
they do not start a new sign-in, request interactive consent, or select another
account. Acquisition remains silent, and interaction-required or unavailable
scope acquisition fails without a sign-in fallback.

Scope acquisition is not principal verification or permission to contact a
workload host. A provider can expose the scope while returning a token format
that this verifier does not support. Managed-identity provider support, in
particular, does not imply that every managed-identity token satisfies the
selected-principal contract.

## Verified principal contract

The verifier acquires one Fabric token and then one PBI token from the same
injected credential. It verifies both signatures using `ring` RS256 and
requires the same tenant, directory object, selected account, and user or
application kind. Caller selection metadata and decoded claims alone are
never principal proof.

The owner selection uses `Audience::Fabric`. The tenant is a canonical,
nonzero, lowercase, hyphenated globally unique identifier (GUID). The account
is either an explicit ASCII sign-in name containing `@` or a canonical
application client GUID; opaque account labels are not supported here.

### Supported signed claims

Only signed, three-part JSON Web Tokens (JWTs) with `alg=RS256` and
`ver` equal to `1.0` or `2.0` are supported. The token must contain a bounded,
nonempty `kid`; an optional `typ` must equal `JWT`. Token-supplied key locations,
embedded keys, and unsupported critical-header behavior are rejected.

| Evidence | Required value or relationship |
| --- | --- |
| `aud` | Exact URL audience: `https://api.fabric.microsoft.com` or `https://analysis.windows.net/powerbi/api`, without `.default` or a trailing slash |
| `tid`, `oid` | Selected canonical tenant GUID and canonical nonzero directory-object GUID |
| `iss` | Exact selected-tenant issuer for the signed token version |
| `exp`, `nbf`, `iat` | Required integer times satisfying the verifier's validity and lifetime checks |
| Selected user | Signed `upn`, `preferred_username`, or `unique_name`, in that precedence order, matching the selected ASCII sign-in name without case sensitivity |
| Selected application | Mandatory signed `idtyp=app` and matching canonical `azp` or `appid` client GUID; both client claims must agree when present |

For version `1.0`, the issuer is
`https://sts.windows.net/<tenant_id>/`. For version `2.0`, it is
`https://login.microsoftonline.com/<tenant_id>/v2.0`. A user selection rejects
`idtyp=app` and `xms_mirid`; an application selection never treats an `appid`
claim without signed `idtyp=app` as sufficient evidence.

Token and provider expiry must each leave more than 60 seconds. The verifier
rejects future `nbf` or `iat`, times after `exp`, expiry more than 24 hours
ahead, and an `exp` minus `nbf` interval over 24 hours. The verified pair expiry
is the earliest signed or provider expiry across both tokens.

Opaque tokens, encrypted tokens, missing identity evidence, GUID-resource
audiences, and unsupported cloud authorities fail closed. This is a deliberately
bounded token-format contract, not support for every Microsoft Entra ID access
token format. The verifier does not alter claims or relax checks to accommodate
an unsupported token.

### Issuer keys and bounded refresh

Key retrieval uses the shared HTTP engine without a bearer token. The verifier,
not the token or caller, constructs the selected-tenant public Microsoft Entra
ID JSON Web Key Set (JWKS) endpoint:

- Version `1.0`: `https://login.microsoftonline.com/<tenant_id>/discovery/keys`.
- Version `2.0`: `https://login.microsoftonline.com/<tenant_id>/discovery/v2.0/keys`.

The selected key's issuer must match the exact known issuer after substituting
the selected tenant into an allowed `{tenantid}` template. Key type must be
`RSA`; optional algorithm, usage, and operations must permit only the expected
RS256 signature verification. No token-provided discovery endpoint is followed.

The verifier holds at most two issuer-version documents in memory. Each document
contains from 1 through 32 uniquely identified keys and at most 128 KiB of decoded
response bytes, enforced while streaming and again before parsing. Documents
have a one-hour cache lifetime.

One bounded refresh is allowed on an initial `kid` miss, then at most one
key-miss refresh attempt per five minutes for that document. Failed or cancelled
refresh attempts also consume that interval.

A signature failure never triggers a refresh. Retrieval has no redirects or
automatic retries. No keys or principal proofs are persisted.

## Explicit targets and origin checks

The caller supplies all selection before credential or network access:

- Select the account and tenant in `CredentialSelection`.
- Select the environment and matching catalog origin.
- Select the exact cluster and workload origins independently.
- Select canonical capacity, workspace, and notebook GUIDs in `NotebookReadScope`.
- Complete target, request, response-limit, and output preflight.
- Supply the original operation deadline and cancellation policy.

`NotebookWorkloadSelection::new` freezes the owner, environment, catalog,
cluster, workload, and resource scope. `NativeNotebookExecutor::new` requires
the same owner selection in `RequestContext`, a Fabric service origin, equal
context and HTTP endpoint bindings, and that binding's exact catalog origin.
Construction performs no token acquisition or network request.

`NotebookEnvironment::Production` requires the public catalog origin
`https://api.fabric.microsoft.com`. Other declared environment variants retain
additional verified-account restrictions; they are not public-cloud
qualification evidence, sovereign-cloud support, or permission to infer a
catalog from an account. No alternate environment or catalog is attempted.

### Raw origin boundaries

`NotebookOrigin::new` validates raw input before URL parsing. It accepts a
lowercase `https://` origin with lowercase ASCII Domain Name System (DNS) labels,
default port 443, optional literal `:443`, and an optional root slash. It rejects
IP addresses, user information, non-root paths, queries, fragments, escapes,
trailing dots, backslashes, alternate port spellings, and normalization tricks.

Bootstrap responses confirm selections; they never replace them. Response
origins receive the same raw authority and path checks, allowing case-insensitive
ASCII DNS names for exact origin comparison. Scheme, port, path, user information,
query, and fragment restrictions are not normalized away.

## Bootstrap and workload authorization

Each read performs its own verified token acquisition and workload-token
exchange. The adapter does not persist or reuse a workload grant across reads.
Only the verifier's bounded issuer-key cache is retained by this adapter.

1. Verify the selected Fabric and PBI principal and check remaining validity.
2. Send `GET /metadata/cluster` to the selected catalog with the verified PBI bearer token.
3. Require HTTP 200 and a `backendUrl` that confirms the exact selected cluster.
4. Send `POST /metadata/v201606/generatemwctoken` to that selected cluster with the same verified PBI bearer token.
5. Validate every required grant field against the selected workload, capacity, and lifetime.
6. Send the one allowed workload GET with `Authorization: MwcToken <workload_token>`.

The exchange body has exactly these fields and selected values:

```json
{
  "capacityObjectId": "<capacity_id>",
  "workspaceObjectId": "<workspace_id>",
  "workloadType": "Notebook",
  "artifactObjectIds": ["<notebook_id>"]
}
```

Replace the resource markers with the previously selected canonical GUIDs.
The exchange requires HTTP 200 and nonempty `Token`, `TargetUriHost`,
`CapacityObjectId`, and RFC 3339 `Expiry` fields; their lower-camel-case aliases
are also accepted. A token must contain only ASCII graphic characters and be
at most 32 KiB.

The returned workload host and capacity must match the selected values.
Expiry must leave more than one second, be no more than one hour ahead, and
not exceed either verified access-token expiry or either provider expiry.
Missing, foreign, malformed, or overlong grants fail before workload access.

The PBI bearer token is used only for the two bootstrap endpoints. The workload
request uses only `MwcToken`, never a Fabric or PBI bearer token. Grant fields
cannot expand the caller's selected resource scope.

## Allowed Notebook reads

Every leaf route starts with this exact selected scope:

```text
/webapi/capacities/{cap}/workloads/Notebook/Data/Automatic/api/workspaces/{ws}/artifacts/{note}
```

| Method | Suffix | Query |
| --- | --- | --- |
| GET | `/content` | None |
| GET | `/jupyterApi/versions/1/api/sessions` | None |
| GET | `/jupyterApi/versions/1/api/sessions/active` | None |
| GET | `/jupyterApi/versions/1/api/sessions/{id}` | None or exactly `detailed=true` |

Requests must declare `NativeEffect::Read`, use the exact selected workload
origin, have no body or custom headers, and retain `application/json` for both
Accept and Content-Type metadata. Exact session identifiers contain from 1 through
128 ASCII letters, digits, hyphens, or underscores; `active` remains the
collection selector, not an exact-session identifier.

The executor rejects HEAD, mutation methods, compute operations, other routes,
extra query parameters, and foreign resources before token acquisition.
Existing `NotebookRuntime` methods outside this subset do not gain
authorization from this adapter. In particular, existing HEAD request builders
do not establish HEAD support here.

Use the existing content and session decoders for response interpretation.
The adapter returns observed workload status, bounded body bytes, and validated
response metadata; it does not turn a failed or partial read into empty success.
It does not add paging, output rendering, session readiness inference, or a
second content engine.

## Deadline, cancellation, and finite work

One externally supplied `NativeDeadline` covers token acquisition, key retrieval,
catalog validation, token exchange, and the workload response body. Every HTTP
step uses the remaining original budget, and the existing injected timer races
the complete read. No phase restarts the operation deadline.

`execute_cancelled` also races the caller's cancellation future, with
cancellation checked before authentication or HTTP is polled. Dropping the
operation future releases local pending authentication, key retrieval,
bootstrap, and response observation. It does not revoke issued tokens, send a
remote cancellation request, or roll back an exchange already sent.

| Surface | Bound or policy |
| --- | --- |
| Encoded access token | At most 32 KiB |
| JWKS document or bootstrap response | At most 128 KiB |
| Workload grant token | At most 32 KiB |
| Workload response | Caller-selected nonzero limit, at most 16 MiB |
| HTTP dispatch | One send per step; no redirect, status retry, or exchange replay |

The caller-owned HTTP client must disable redirects and automatic retries,
stream bounded bodies, and release local work when dropped. Failure does not
authorize another exchange or an inferred target. Tokens, grants, bodies,
selected identifiers, and provider diagnostics are not logging material;
adapter diagnostics remain redacted or sanitized.

## Local API composition

This example composes already validated, explicitly supplied inputs. It does
not acquire credentials, select a target, or contact a service. Pass a context
containing the opted-in credential and the public Fabric binding, and retain
the deadline created for the original operation.

```rust
use fabric_core::RequestContext;
use fabric_sdk::runtime::{NativeDeadline, NativeError};
use fabric_sdk_http::{
    HttpExecutor,
    notebook_workload::{
        NativeNotebookExecutor, NotebookEnvironment, NotebookOrigin,
        NotebookReadScope, NotebookWorkloadSelection,
    },
};

fn compose_public_reads(
    context: RequestContext,
    http: HttpExecutor,
    cluster: NotebookOrigin,
    workload: NotebookOrigin,
    scope: NotebookReadScope,
    deadline: NativeDeadline,
) -> Result<NativeNotebookExecutor, NativeError> {
    let selection = NotebookWorkloadSelection::new(
        context.selection().clone(),
        NotebookEnvironment::Production,
        NotebookOrigin::new("https://api.fabric.microsoft.com")?,
        cluster,
        workload,
        scope,
    )?;
    NativeNotebookExecutor::new(context, http, selection, deadline)
}
```

Before moving the selection into the executor, `selection.workload_endpoint()`
and `selection.scope()` provide inputs for the existing `NotebookRuntime`
builder. Use `content_get_request`, `sessions_request`, or
`session_get_request` for the allowed routes. The caller still owns output
preflight and must not extend the deadline between composition and dispatch.

## Provenance and qualification

The separately verified, read-only protocol contract records revision
`e250d5d32b04efcc5623a414a361727a93c271ce` and these source-file areas.
This is protocol provenance, not a copied implementation or a public REST
support declaration. The Rust implementation is independently written from
the task contract using existing Rust libraries.

| Source file | Protocol source mapping |
| --- | --- |
| `auth/mwc_token.py` | Workload-token bootstrap and exchange |
| `auth/identity.py` | Identity and audience contract |
| `environment.py` | Explicit environment selection |
| `constants.py` | Protocol constants |

The offline fixture under `rust/tests/fixtures/notebook-identity/` uses a generated,
never-enrolled test signing key and signed synthetic tokens. The workload test
target drives the real verifier and shared HTTP path through injected offline
credentials and responses. No fixture requires live Microsoft Entra ID,
Fabric, or workload calls.

The current source test inventory includes route and selection guards, signed
principal mismatch, unsupported token formats, key bounds and refresh,
grant validation, no replay, bounded bodies, deadlines, cancellation, and
redacted diagnostics. An authored test is not a passed test. No build, test,
platform proof, or live qualification result is claimed by this document.

Release evidence must distinguish three separate states:

- **Source availability:** The opt-in implementation and fixture exist under development.
- **Public release:** Publication and supported feature availability require separate release evidence.
- **Platform qualification:** Actual Windows, macOS, and Linux behavior, identity policy, and service authorization require applicable validation evidence.

No platform qualification or release readiness is established for this
foundation. Authentication/core review, response-origin contract reconciliation,
failed-refresh throttling, feature-enabled validation, and applicable platform
and service acceptance remain gaps. No Notebook CLI route or broader workload
capability is implied.

## Related documentation

- [Shared HTTP execution](../engine/core/http/executor/README.md).
- [Identity adapters](../engine/core/auth/provider/README.md).
- [Endpoint and request context](endpoint-context.md).
- [Fabric-only runtime adapter](native-runtime-http.md).
- [Notebook runtime contract](notebook-runtime-native.md).
