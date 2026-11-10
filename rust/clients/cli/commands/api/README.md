<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Raw API commands

`fabric-cli-api` is the canonical CLI-layer owner of raw API command behavior, a
sibling of `fabric-cli-access` and `fabric-cli-catalog`.

## Belongs here

- `ApiInput` and `read_api_input`: inline JSON and bounded explicit local-file
  loading, including the existing multipart framing checks.
- `ApiOptions` and `build_api_request`: source parameter/header pairs, supported
  verbs and explicit origin/request bounds, validated before credentials.
- `api`: caller-selected SDK execution and source status/header/text projection.
  HTTP error responses remain data; transport and capability failures are errors.

## Does not belong here

Typed service endpoints, authentication, retries, HTTP execution engines, mixed
parser/host composition, ACL/label handlers and catalog algorithms do not belong
here. `RawApiCommand` and `ApiClients` keep their old nominal shapes in
`fabric-cli-resources::frontdoor` (the compatibility namespace); this package
does not depend on that facade.

`CommandError` and `CommandValue` reexport the shared definitions in
`fabric-cli-command-contracts`. That neutral package also retains the single
`ResourceHttpExecutor` adapter over shared HTTP policy. There is no second
transport or authentication stack. Callers select origins, sessions, input files,
MIME boundaries, limits and cancellation. The existing pre-credential header,
filename, allocation and audience safety rules remain unchanged. No ambient
credential/runtime/configuration discovery or process exit is performed.

## Provenance and verification

`src/api.rs` was moved intact from `fabric-cli-resources`. The Python baseline is
`microsoft/fabric-cli@cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Independent expectations remain in `rust/tests/compatibility/raw_api/`.
Package-local tests/examples stay in `fabric-cli-resources` at
`../../compatibility/resource-commands`, exercising the original imports and
shared-HTTP bindings; its extraction-identity test also uses this owner directly.
No expected fixture was regenerated. This is not a new parser, a general
multipart serializer, or evidence of live service/platform parity.
