<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared bounded token paging

`TokenPager` is a transport-neutral SDK-layer collector, coordinated as a
prerequisite for the job family. Its first production consumer is
`JobClient::list`; existing workspace/item/folder APIs and oracles remain
unchanged. Other service families can inject their own async fetch callback.

The collector reuses the existing `Page<Value>` and response wire types.
It owns no HTTP client, credential provider, retry, LRO or runtime. The fetcher
receives a validated `PageRequest` and must enforce the response bound while
streaming. Services remain responsible for initial endpoint authorization.
The collector binds continuation work to that HTTPS origin and exact raw route;
returned URIs are checked when active but never followed.

Python authority is `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`fab_api_client::_handle_successful_response` and `ApiResponse.append_text`.
The new job-family reference executes those unchanged functions, including
three-page recursion. Header presence takes priority over body tokens even for
an empty `x-ms-continuation` header. Opaque tokens retain whitespace and encode
once using Python `quote_plus` semantics, including unescaped `~`.
Duplicate query keys replace values in the first position. Empty/falsey body
tokens end traversal; terminal unused URI values are ignored.

`PagedValues` reports actual committed pages, ordered rows with duplicates, and
the next unconsumed token when known. Python's recursive `total_pages=1`
metadata is not treated as an accurate page count. `PagingError<E>` retains
typed fetch errors and complete earlier pages; a failed/incomplete list is
never an empty or partial success.

Nonzero page/item/byte bounds, repeated-token detection, active URI
origin/route validation, typed malformed-response failures and cancellation
are explicit SDK safety behavior. A page exceeding the row bound is not partly
committed. Cancellation wins ready fetch results and drops in-flight work.
Debug output excludes URLs, opaque tokens and row contents.

The independent source cases and explicit safety cases are separate categories
in the public integration tests. No existing service pager is migrated here.
