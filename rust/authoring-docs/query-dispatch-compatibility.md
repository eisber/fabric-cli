<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Opt-in dispatched-query evidence

Source authority: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`client/fab_api_client.py::do_request` and `_handle_successful_response`.
This is an additive read-evidence API, not automatic request-state compatibility
or a change to ordinary stateless SDK behavior.

## Source lifetime versus execution evidence

Python obtains `request_params = getattr(args, "request_params", {})`, mutates it
with URI pairs, then inserts an active continuation token before acquiring a
credential and dispatching the request. A preexisting dictionary therefore keeps
the last inserted token after recursive pages. With an absent attribute, each
call's temporary default dictionary has a different lifetime. Clearing, replacing
or removing the attribute, and a later URI key override, change subsequent calls.

`Collection::next_token` is the next **unconsumed** token. It is often absent on
successful completion, and may differ from the query of the last executed read.
A failing credential on page two illustrates an additional distinction:
Python's dictionary is already mutated, but the page-two request was never
dispatched. A trace of actual dispatch cannot claim to be that earlier mutable
dictionary. Compatibility consumers must retain the source's command-specific
dictionary ownership/reset rules and preserve failure/early-exit behavior.

## Public API

`fabric_sdk_api::QueryDispatchTrace::default()` creates a fresh caller-owned
trace. `ApiClient::with_query_dispatch_trace(trace.clone())` opts one client into
it; `CatalogClient` provides the same additive builder. `last_dispatch()` returns
an owned `DispatchedQuery` snapshot or `None`, and `clear()` is an explicit caller
reset. An opted-in client clone shares the supplied trace; cloning an ordinary
client does not retroactively opt it in.

`DispatchedQuery::{origin,method,url,encoded_query,query_pairs}` expose the last
actual validated `Request` passed to the injected executor, after capability,
audience and credential checks. The original encoded query is retained; decoded
pairs preserve order and decode opaque values exactly once. Debug omits the
URL/query. No credential, request body, header, response token, or history is
retained. Original request/continuation bounds still apply.

`CatalogClient::list_hidden_with_query(collection, initial_pairs, limits, cancel)`
adds explicit initial query pairs without mutating them. Existing `list_hidden`
passes an empty slice and its signature/results are unchanged. Existing
`Collection`/`CollectionError` fields, struct literals and defaults are unchanged.
All pagination uses the existing `TokenPager` and original response bytes.

The event is a **logical executor dispatch**, not proof of a socket write,
provider receipt, acceptance, response or side effect. Retries remain inside
`HttpExecutor`; the trace is not a per-attempt wire telemetry hook. Tests compare
these observations with real shared-executor requests received by an injected
Azure HTTP client. Reconciliation GETs from the shared operation adapter are
observed through the same trace if that client is explicitly opted in.

## Boundaries

- Before-dispatch cancellation, unsupported capability, wrong audience or
  credential failure leaves the prior snapshot intact. A fresh trace remains
  empty. A scheduled but unpolled continuation is never reported as dispatched.
- After-dispatch HTTP/stream/byte failures and cancellation retain the dispatched
  query, while the existing typed error and complete partial pages remain intact.
- The caller owns command/identity scope and serial request ordering. Do not
  share a trace across unrelated commands/accounts or infer ordering between
  concurrent requests. Clearing does not cancel active work.
- No request automatically inherits a traced query. An explicit CLI compatibility
  decorator may select pairs for only its source-authorized paired operation;
  this API grants no replay, origin, credential or mutation authority.

Independent source fixtures cover preexisting/absent dictionaries, URI overwrite,
clear/replace/remove, single/three pages, header precedence/empty header, page-two
service/credential failure and interruption. Public consumers additionally test
before/between/in-flight cancellation, unconsumed tokens, original-byte limits,
default-client isolation and redacted Debug. The paired managed mutation
decorator and mutation query builder remain their separately assigned owners'
work; this read API alone does not close those end-to-end parity gaps.
