<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline Rust fixtures

`workspaces-page.json` is synthetic Microsoft-authored data under the repository
MIT license, not a captured tenant response. It contains no credentials or real
workspace identifiers. The URI and opaque continuation token are serialization
examples only and are never requested.

The SDK tests use this fixture to preserve camel-case wire names, item data,
and opaque continuation values. A terminal page and malformed responses are
covered inline. Future service/deployment compatibility fixtures belong here
only after sanitization and source/license review. Tests must remain offline
and must not require `az`, `azd`, Python, or tenant credentials.
