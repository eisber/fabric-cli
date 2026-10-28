<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access, catalog and API commands

`fabric-cli-resources` composes ACL, label, FIND and raw API commands over focused
SDK clients, context resolution, query projection and shared output.

## Register and execute

Use `frontdoor::register(root)`, `AccessCommand::from_matches` and `AccessHost`
in the central command parser. Run `frontdoor::execute_local` before remote
account setup so configured local label listing does not require credentials.

Supply resolved supported resources, configuration, timestamp, execution,
confirmation, warning logging and writers. `ResourceHttpExecutor` adapts the
shared HTTP policy rather than installing another authentication or retry stack.

## Operations

- `AclRead` and `read_acls` combine SDK enumeration and projection.
- `NamedResource`, `AclChange`, `LabelChange` and `MutationOptions` describe
  mutations. Force skips confirmation, not validation or cancellation.
- `list_local_labels` reads an explicit configured path; `labels_from_bytes`
  accepts already-loaded data without discovering configuration.
- `find` searches the catalog. `find_interactive` writes each page before
  requesting continuation through injected confirmation.
- `api` accepts explicit request input and retains raw HTTP status, headers
  and body. Transport and capability failures remain errors rather than data.
- `read_hidden_metadata` retains enriched and internal metadata with diagnostic
  evidence for the caller.

`export_acls` requires `AclExportSink`; it never substitutes a no-op writer.
The host remains responsible for actual storage export.

## Output and evidence

`CommandValue::write_json` writes results to stdout and diagnostics to stderr.
Empty reads remain silent where specified. `CommandError` retains earlier
diagnostics and warnings.

`FamilySurface`, `FamilyPresentation`, `render_family_output` and
`render_family_error` compose shared text or JSON output. The caller supplies
Unicode width, presentation and streams. Writer errors preserve partial output
and do not replay requests.

No credential, runtime, browser, terminal or global configuration discovery is
performed by this library. Resource-specific support checks and missing
capabilities fail explicitly.
