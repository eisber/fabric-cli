<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Workspace listing contracts

Fixed synthetic cases cover empty listings, canonical workspace names/types,
ordering, pagination, query projection, errors and explicit root resolution.

The reference verifies the recorded source tree and fixtures before execution.
Each observation uses isolated context, credentials, HTTP, clock and headless
output. Real networking, application file access, subprocess creation, sleeping
and ambient identity discovery are denied during guarded execution.

Assertions retain exact output bytes, field order, line endings, scopes,
request order, error mapping and unchanged current context. Structured data is
checked within the full JSON envelope, not in place of it.

Headless rendering is a controlled output path. These cases do not establish
interactive terminal styling, authentication, persisted context or every root
parser option.

Normal runs compare immutable expectations and provide no record, update or
regeneration mode. Native SDK and command consumers independently use the same
contracts. Save actual observations and platform versions as validation
artifacts rather than replacing fixtures.
