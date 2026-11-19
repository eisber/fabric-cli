<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access, catalog and API commands

`fabric-cli-resources` is a source-compatible facade over focused command
capabilities. It no longer owns their grammar, preparation or execution:

| Package | Ownership |
| --- | --- |
| `fabric-cli-access` | ACL/label grammar, conversion, reads, mutations and export |
| `fabric-cli-api` | Raw API grammar, conversion, bounded input and execution |
| `fabric-cli-catalog` | FIND, hidden-resource discovery and Domain sync |
| `fabric-cli-command-contracts` | Existing shared values, errors, confirmation and presentation |

The root parser registers the capability schemas directly; app family routing
selects their execution adapters using the same caller-owned session. This
preserves v2 checkpoint `61c8221914152e231e5e64f7bd38418f66264999`, including
all v2-only aliases, help and output rules. The layout at v1 commit
`f8cb97e92eab669785954335e0e7276d30a3377e` is prior art, not a bulk merge.

## Register and execute

Existing consumers can keep `frontdoor::register(root)`,
`AccessCommand::from_matches` and `AccessHost`, including their public struct
literals. These adapters only translate and route to the focused packages.
New consumers can use each capability's public parser and execution API
directly without depending on this facade. Run `frontdoor::execute_local` before remote
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

## ACL command usability

`fab-rs acl grant` is a discoverable alias of `acl set`; `acl revoke` is an alias
of `acl rm`. Existing `set`, `rm` and `del` spellings remain available. Identity,
role, force, confirmation and error handling are unchanged. Revoke removes the
matching assignment; it does not accept a role selector. These aliases do not
add item mutation support or expose ACL commands in the grouped `fab-cli` host.

Read targets are workspaces (`Demo.Workspace`) or supported items
(`Demo.Workspace/Book.Notebook`). An absolute target starts with `/`; an item
can be relative to the current workspace. Use `fab-rs ls /` to discover workspace
names. Workfolders, OneLake paths, SemanticModel, Environment and Dashboard
targets are not supported by this front door. Item reads require Fabric
administrator permission.

The root executable reports a missing `acl ls`/`get` target on stderr with
concise usage, a synthetic example and exit 2, before identity or network setup.
An explicit `--output_format json`, or saved JSON setting, retains the existing
sanitized JSON argument-error envelope. No other command's default is changed.

For normal text output, the root uses `AccessHost::execute_for_output` with its
effective format. Unqueried `acl get` then shows complete ACL record values using
the shared structured text renderer, without table truncation or field loss.
This is an owner-requested ACL-only UX exception to the Python field-discovery
default, not a change to the frozen oracle or `read_acls`/`AccessHost::execute`.
Explicit JSON, tabular export and explicit queries retain their existing contract.
Use `-q '.'` to retrieve records in JSON or `-q ''` to explicitly discover field
paths. Paging, source JSON empty-result behavior, collection bounds and provider errors remain
unchanged. Separate offline UX tests cover the exception alongside the unchanged
Python fixture tests.

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

## Grouped v2 service adapter

The additive `service` module exposes `service domain sync PATH --role Contributor`
through `service_command()`, `parse_args`, caller-owned `DomainResolver`, neutral
operation preparation and immediate `ApprovalPolicy`. `execute` uses the SDK's
single native POST; `write_report` retains its receipt on output failure.
It does not mount this action in the existing v1 front door or install a production
resolver, credential provider or default approval.
See [the service contract](../../../../authoring-docs/service-domain-sync.md).
