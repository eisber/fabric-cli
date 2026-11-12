<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline Notebook identity fixtures

`rsa-test-key.json` is a newly generated, never-enrolled RSA-2048 test key. It
has no tenant, service, account, certificate, or production trust relationship.
The fixture intentionally includes its test-only private signing material so
offline tests can sign short-lived synthetic claims using `ring`. Never use it
outside tests. No production tokens or credentials were captured.

`protocol.json` freezes the selected-resource request shapes independently of
the adapter. The protocol reference is revision
`e250d5d32b04efcc5623a414a361727a93c271ce`, `auth/mwc_token.py`,
`auth/identity.py`, `environment.py`, and `constants.py`. These are independently
authored synthetic observations, not copied implementation source or comments.
Explicit target selection, cryptographic principal verification, required grant
fields, bounded expiry, no retries, and read-only routing are intentional native
constraints. Source proof does not establish service or platform qualification.

The HTTP suite uses the production verifier and existing shared HTTP engine.
The CLI-layer support test composes the official native headless credential
with that same verifier and engine, respecting the existing architecture layers.
Both targets require explicit features and are registered in the impact graph;
neither an empty default-feature test target nor a decode-only verifier counts
as evidence.
