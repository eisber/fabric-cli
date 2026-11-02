<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Token-based collection paging

`TokenPager` collects ordered service rows through a caller-supplied asynchronous
fetch callback. It owns no HTTP client, credentials, retry policy or runtime.

`new` selects the `value` collection field.
`with_collection_field` selects another literal field such as `accessDetails`
or `domains`. Missing or non-array fields fail explicitly without falling back
to another collection.

## Traversal policy

The fetcher receives a validated `PageRequest` and must enforce streaming
response limits. Initial endpoint authorization belongs to the service client;
continuations stay bound to the same HTTPS origin and exact route.

Header continuation takes priority over a body token, including an empty
header that ends traversal. Tokens remain opaque and are encoded once.
Duplicate query keys replace values in their first insertion position.
Inactive terminal URI metadata is not followed.

`PagedValues` retains ordered duplicate rows, actual committed page counts and
the next unconsumed token when known. `PagingError<E>` retains the typed failure
and complete earlier pages. A failing page is never partly committed or returned
as complete success.

Nonzero page, row and byte bounds, cycle detection and active-link validation
are mandatory. Cancellation wins a simultaneously ready fetch and drops
in-flight work. Debug output excludes URLs, tokens and row contents.
