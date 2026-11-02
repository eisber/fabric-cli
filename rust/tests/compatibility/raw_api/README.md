<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Raw API contracts

Synthetic fixtures check request builders, explicit origin/audience selection,
URL and body bytes, status, headers, raw text, query projection and output.
The reference injects credentials, context, sessions and time without
initializing the application or contacting a service.

Native consumers use the shared `execute_bound` engine for all supported public
origins and safe headers. Negative cases reject mismatched audiences and
authentication, host, cookie or proxy-header overrides before credentials.

Transport failures remain distinct from service error responses.
Multipart/file and allocation tests enforce additional native safety policy;
they are not a general multipart serializer specification.

Normal reference runs compare immutable expectations. Initial evidence capture
refuses overwrite, and native observations never replace expected values.
