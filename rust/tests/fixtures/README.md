<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline Rust fixtures

`workspaces-page.json` is synthetic Microsoft-authored data under the repository
MIT license, not a captured tenant response. It contains no credentials or real
workspace identifiers. The URI and opaque continuation token are serialization
examples only and are never requested.

The SDK tests use this fixture to preserve camel-case wire names, item data,
and opaque continuation values. A terminal page and malformed responses are
covered inline. Additional service and deployment fixtures require sanitization
and license review before inclusion. Fixture-based Rust tests remain offline
and do not discover credentials or require developer authentication tools.
