<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental Windows broker lifecycle

This optional native boundary backs `fabric-identity/windows-broker`. It does
not select a production identity backend or establish MFA, Conditional Access,
guest-account, alias, packaging or service acceptance. Authentication/core team
review and separately authorized native and live evidence remain required.

## Explicit interactive account selection

The Windows-only native `select_account(client, tenant_hint, account_hint,
interaction)` API supports an explicitly requested login with incomplete account
or tenant selectors. Both hints are optional; the client registration must still
be a GUID. Hints are validated before provider access. `Interaction::Silent`
returns `InteractionRequired` without enumeration, token requests or UI, even
when a cached account might exist. Fully specified callers should construct
`EntraOptions` directly and use the unchanged credential login API below.

Hosts can call the pure Windows-only
`validate_selection(client: &str, tenant: Option<&str>, account: Option<&str>)`
before reading session settings or contacting WAM. Missing hints are valid;
present malformed/empty hints return sanitized `InvalidConfiguration`, never a
saved-identity fallback. This performs no environment discovery, state-file I/O
or provider work and grants no interaction permission. `select_account` reuses
exactly this validation.

Allowed selection always requests WAM-controlled interactive `select_account`
UI, never the first cached account, the default Windows persona, or a tenant
inferred from a UPN domain. It uses only the Microsoft work/school provider
(`https://login.microsoft.com`), with the provider's `organizations` selector
when no tenant is given, or the explicit tenant GUID otherwise. The request
authority is `https://login.microsoftonline.com/organizations` or the explicit
public-cloud tenant URL, with `validateAuthority=yes`, `wam_compat=2.0` and a
`LoginHint` only when supplied. The request uses the existing desktop interop
and default prompt for a bare picker, retaining the forced prompt only when an
explicit UPN hint is supplied. No consumer-provider, sovereign-cloud,
browser or other credential fallback is attempted.

Selection requests only Fabric `.default`, `openid`, `profile` and
`offline_access` as bootstrap scopes. Exactly one returned `WebAccount` must
have a nonempty ID and valid UPN, nonempty token, future authoritative
`TokenExpiresOn`, and a validated public-cloud response `Authority`. A GUID
authority supplies the resource tenant. For a bare `organizations` request only,
the exact same neutral response authority may instead use
`WebAccount.Properties["TenantId"]` as a home-tenant bootstrap candidate. That
property is used by Microsoft's [MSAL WAM account adapter](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/0661b34a41255bfdd7980663572be0544578117e/src/client/Microsoft.Identity.Client/Platforms/Features/WamBroker/AadPlugin.cs#L216-L235);
it is not proof of the bootstrap token's resource tenant. The existing
tenant-bound login still has to validate all three audiences before any saved
selection. Personal-account tenant metadata is rejected. Explicit UPN
and tenant hints must match, ignoring ASCII case; guest selection remains bound
to the explicitly requested target tenant. Missing/ambiguous metadata is an
error, never permission to guess a tenant or decode an access token.
An explicit GUID authority remains authoritative for guest resource-tenant
selection; the account's home `TenantId` cannot replace it.

Missing/invalid metadata reports only a fixed field name (such as `Authority`,
`TenantId` or `TokenExpiresOn`), never its provider value. CLI errors separately
identify account selection and subsequent selected-session authentication.

The bootstrap token is discarded. The result is only validated, non-secret
`EntraOptions`, not a successful session or a native-account pin. The caller must
then use the existing coordinated `WindowsCredential::login(options, true)` to
validate Fabric, OneLake and ARM and establish its native account binding.
Same-UPN ambiguity at that stage still fails closed. Status, logout, SDK refresh
and ordinary command paths must never call selection. The API does not write
session settings or expose account lists; WAM may update its own secure cache.

Selection shares the existing worker pool, cancellation and 600-second
interactive native deadline. WAM is parented to the visible console/terminal root
owner when available; the borrowed HWND is never destroyed. Otherwise an owned
visible sign-in window is retained through native completion. Its private window
procedure turns both posted and sent close requests into cancellation, without
destroying the modal parent early. Callers must bound their
local async wait; dropping the operation requests cancellation but does not
claim native teardown. Native resource ownership and cleanup guarantees below
apply on provider failures as well as success.

`select_account_with_progress` reports only provider lookup, interactive waiting
and response validation. Hosts can include those phases in timeout diagnostics;
no account identifiers, provider response text or tokens cross that callback.
The [documented WAM parent-window requirement](https://learn.microsoft.com/en-us/entra/msal/dotnet/acquiring-tokens/desktop-mobile/wam#parent-window-handles)
also covers console/Windows Terminal root-owner handles.

The provider selectors and interactive request properties follow the Microsoft
WAM [provider factory](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/4.30.1/src/client/Microsoft.Identity.Client/Platforms/Features/WamBroker/WebAccountProviderFactory.cs),
[AAD request builder](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/4.30.1/src/client/Microsoft.Identity.Client/Platforms/Features/WamBroker/AadPlugin.cs)
and [prompt forwarding](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/4.30.1/src/client/Microsoft.Identity.Client/Platforms/Features/WamBroker/WamBroker.cs).
The public client registration must support the requested organizational tenants
and the WAM redirect URI
`ms-appx-web://Microsoft.AAD.BrokerPlugin/{client_id}`; see the Microsoft
[desktop WAM guidance](https://learn.microsoft.com/en-us/entra/identity-platform/scenario-desktop-acquire-token-wam).
This API does not create registrations, alter consent or prove live acceptance.

## Credential API and local logout

The existing `WindowsCredential::login(options, allow_interaction)`,
`WindowsCredential::connect(options)`, Azure `TokenCredential::get_token` and
`fabric_identity::sign_out(options)` signatures are unchanged. Only explicit
login permission can authorize UI. Connect requests Fabric only. Login requests
Fabric, OneLake and ARM in order, and publishes no partial credential. SDK
refresh remains silent and native-account pinned. Metadata is historical, not a
fresh status check. SDK credentials remain caller-injected; SDKs do not depend
on this identity implementation.

`WindowsCredential::status(options)` silently checks all three login audiences
using read-only lifecycle admission: it cannot reopen signed-out or unconfirmed
state and rejects those states before broker work. It is not equivalent to
`login(options, false)`: the latter is an explicit login that may reopen confirmed
sign-out while still forbidding UI. Interaction permission never determines
whether an action is an explicit login. CLI hosts must route status to `status`,
not to a no-interaction login.

These credential entry points share a process-local coordinator. Its keys are
the fixed public-cloud provider namespace, explicit tenant/client GUIDs and
ASCII-case-folded UPN. Tenant keys sharing provider/client/UPN join one
operation/invalidation domain **before** their first acquisition. Validated
equal native IDs join same-client domains conservatively, without releasing
active native reservations or discarding logout generations. No alias discovery
or guest mapping is performed.

Logout linearizes on its first poll: it invalidates existing credentials before
waiting for native work. Constructing and dropping an unpolled logout does
nothing. Token/credential publication linearizes at the final locked domain and
native-ID generation check, not at a broker callback. A success buffered before
logout but not yet published is rejected. A token published before that check
cannot be recalled.

Logout cancels prior matching operations and waits up to five seconds for actual
teardown before starting native app/client sign-out. Failure, cancellation,
deadline or unknown cleanup leaves old credentials invalid and local state
unconfirmed. Explicit same-identity logout retry is required before reopening
through explicit login. Silent connect and an old credential cannot reopen the
session. Even a later successful login does not rehabilitate old objects.

Native logout pins the original validated account ID when known. A missing
original account is idempotent absence; a different same-UPN account is rejected
rather than selected. For an identity never acquired locally, lookup still
requires today's unique explicit UPN contract. The broad same-client publication
barrier is transient: confirmed absence means no native account sign-out occurred,
and releases that barrier after actual teardown without reopening the selected
domain. A found native ID retains its precise account/client invalidation.
If logout fails or is dropped, confirmed late teardown likewise releases the
broad barrier, but leaves the selected domain unconfirmed and its old credentials
invalid. Uncertain or still-active cleanup does not expire on a timer.

A retained client publication cutoff rejects acquisitions admitted before
or during unresolved logout **without a previously confirmed native binding**,
including aliases whose native ID arrives only after confirmed absence.
Binding knowledge is captured at admission, not learned from the operation's
own response. A previously bound, distinct principal's overlapping refresh can
publish after confirmed absence without retry or logging in the absent selection.
Precise native-ID and selected-domain invalidation checks always apply, even
to previously bound requests whose ID matches a found logout target.
The cutoff applies to unbound operation admission, not older credential objects.
While cleanup is unresolved, same-client publication fails cleanup-pending.
Other client registrations remain isolated. No equivalence is inferred for an
unknown future principal; alias support and cross-process revocation remain
outside this contract.

This is **in-process application-account invalidation**, not cross-process access
token revocation. WAM sign-out applies to that client/account on this device,
potentially including other programs using the registration. It does not remove
the Windows account, revoke other-device sessions, recall previously returned
access tokens, reset Python CLI settings or guarantee rollback of broker cache
updates. Raw native `acquire`/`logout`/`logout_bound` callers do not participate in
the credential coordinator and must supply their own session policy.

## Bounds, cancellation and pending cleanup

- The coordinator retains at most **64 identity keys**, including signed-out
  tombstones, for the process lifetime. It never evicts invalidation state.
  Registry exhaustion returns `IdentityCapacityExhausted` with restart guidance,
  not interaction-required or "no account". Existing selections remain usable.
- One native operation may be admitted per known application-account domain.
  Concurrent acquisition returns `OperationInProgress`, not an unbounded queue.
- All native spawn paths, including raw interactive requests, share **four**
  worker reservations. A worker owns its permit until its final explicit action,
  after confirmed operation/window/apartment cleanup. Consumer drop, timeout,
  cancellation or early result delivery never releases that permit.
- Native absolute deadlines are **120 seconds** for silent/logout work and
  **600 seconds** for explicitly interactive work, across the entire operation.
  Coordinated credentials also bound local waiting on the caller's Tokio
  runtime, even if a native synchronous call prevents the worker observing its
  deadline. The caller may impose a shorter deadline. No independent async
  runtime is created.
- Cancellation requests WinRT `Cancel` once. After five seconds without terminal
  cleanup, local waiting can return `CleanupPending`; the worker still owns the
  operation, parent ownership and STA. Timeout similarly returns `TimedOut` without
  claiming teardown. Completion notification failure remains
  `CompletionUnconfirmed`, not successful cleanup.
- Late native termination automatically recovers its worker capacity after
  confirmed cleanup. An unconfirmed window/resource cleanup quarantines its
  reservation. Four stuck/quarantined workers cause deterministic
  `CapacityExhausted`, with non-secret active/limit counts. Wait for actual cleanup;
  if it remains unconfirmed, restart this process deliberately. **Do not retry in
  a loop.** No replacement threads, forced thread termination or premature window
  destruction are used to conceal exhaustion.
- Poisoned lifecycle bookkeeping returns `CoordinationFailed` and fails closed;
  it does not repeatedly panic, recover a credential or silently recycle
  uncertain native resources.

WinRT cancellation is cooperative. Native synchronous calls/provider hangs can
prevent the worker from observing its deadline. Raw native API callers must impose
their own local-wait deadline. No bounded OS/provider termination is promised. WinRT
[`Close`](https://learn.microsoft.com/en-us/uwp/api/windows.foundation.iasyncinfo.close)
is not legal before completion. The native owner retains resources until it can
observe terminal completion; it does not force-close to improve availability.

WAM retains secure persistence. This implementation adds no token file, raw
token input, credential fallback, registration/consent operation, provider-field
change or access-token JWT identity proof. Claims-challenge replay and CAE remain
unimplemented; Azure Core 1.1.0 token request method options are not evidence of
those capabilities.

## Offline evidence boundary

Synthetic tests exercise the real private coordinator backing credential entry
points and actual returned credentials' SDK refresh, using controlled broker
results and independent teardown signals. Cases include initial cross-tenant
admission, late native-ID alias merging, buffered success versus logout,
cancelled/failed logout, unpolled logout, retained capacity and late recovery,
registry saturation, scope rejection, poison and redaction. These fixtures never
use a real token provider, account or sign-in UI.

Source changes and test definitions alone are not passing evidence. Record exact
source hashes, feature/platform scope, executed commands/exits and independent
review. Existing tests that really create an STA/hidden window require their
own native execution authorization. Live MFA/CA, guests/aliases, service access
and team approval remain unproved.
