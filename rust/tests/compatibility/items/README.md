<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Item SDK contracts

This suite covers workspace item LIST and generic item GET with synthetic,
hand-authored request and response expectations.

Cases retain opaque identifiers, ordered duplicate rows, future/null types,
Unicode, query encoding, continuation tokens and service error precedence.
`query-encoding.json` additionally checks punctuation and literal query bytes.

Reference execution verifies recorded source/fixture identities and uses
injected sessions, credentials and time. It does not initialize the application,
discover accounts, contact services or write application files.

Native consumers exercise standalone clients, facade delegation and the shared
HTTP adapter. Safety tests enforce bounds, active link validation, malformed
response rejection, cancellation and typed later-page progress.

Generic metadata is separate from definition retrieval, specialized collection
routes and complete CLI presentation. Use the relevant command suites for those
boundaries.

Expected results are immutable. There is no record, update or regeneration mode;
observed outputs belong in separate validation artifacts.
