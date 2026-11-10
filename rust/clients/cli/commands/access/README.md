<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access commands

`fabric-cli-access` is the canonical CLI-layer owner of ACL and sensitivity-label
behavior. It belongs beside `fabric-cli-catalog` and `fabric-cli-api`, not below
the legacy `fabric-cli-resources` aggregate.

## Belongs here

- `AclRead`, `read_acls`, `read_named_acls`, `acl_resource` and `project_acls`:
  resource support checks, source row projection, queries and OneLake guidance.
- `AclChange`, `LabelChange`, `NamedResource`, `MutationOptions`, `mutate_acl`
  and `mutate_label`: validation, probing order, confirmation and SDK dispatch.
- `list_local_labels` and `labels_from_bytes`: bounded, explicitly selected local
  definitions, including missing-configuration warnings and source help text.
- `AclExportSink` and `export_acls`: real caller-owned export, including falsey
  values that ordinary stdout routing would suppress.

## Does not belong here

Generic resource create/update/delete, catalog search, raw API commands, mixed
command parsing, credential discovery, terminal setup and process exit do not
belong here. The old mixed parser/host remains compatibility composition in
`../../compatibility/resource-commands`, not a dependency of this package.

`CommandError`, `CommandValue` and `Confirmation` are reexports of
`fabric-cli-command-contracts`, not new types. Callers retain the same SDK client,
runtime, cancellation, limits, confirmation and output contracts. Force bypasses
questions only; cancellation is not rollback, and unavailable capabilities never
imply consent or successful export. No runtime, terminal, browser, credential or
global configuration is discovered.

## Provenance and verification

This is a mechanical extraction of `fabric-cli-resources`'s ACL/label consumers;
source algorithms and error/output ordering are unchanged. The Python baseline
is `microsoft/fabric-cli@cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Independent expectations remain in `rust/tests/compatibility/access/`.
Package-local tests and examples remain in `fabric-cli-resources` at
`../../compatibility/resource-commands` and exercise these handlers through the
original public imports. Its extraction-identity test also uses this owner
directly. Moving ownership is not new command, native-platform or live parity.
