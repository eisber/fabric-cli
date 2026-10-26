<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Resource command consumers

`frontdoor` now supplies modular Clap builders, `AccessCommand::from_matches`
and a borrowed `AccessHost` for all nine ACL/label/FIND/API leaves. Mount
`frontdoor::register(root)` in the single CLI parser; do not copy
its argument definitions into another binary. See the
[frontdoor contract](../../docs/access-frontdoor.md) and `access_frontdoor`
public consumer example. This library addition does not register the executable.
Run `frontdoor::execute_local` before remote-host/account construction so
configured local label listing never needs an SDK client or account.

This is an injected command library, not a competing front door. `REGISTRATIONS`
describes implemented canonical/alias paths. `AclRead`/`read_acls` compose actual
SDK enumeration, source-specific projection, native JMESPath and existing JSON
output. `ResourceHttpExecutor` adapts the transport-free SDK trait to the shared
Fabric-only HTTP policy without another retry/TLS/auth engine.

The front door supplies resolved supported resources, explicit config, runtime,
timestamps, writers and warning logging. `CommandValue::write_json` keeps
diagnostics on stderr, JSON on stdout and source-silent empty reads silent.
`write_error` preserves the existing envelope/exit-code contract.
`list_local_labels` reads only the explicit configured path;
`labels_from_bytes` supports an injected local-file consumer. Neither reads home
configuration, credentials or environment variables.

The binary, parser and hidden-resource resolver are unchanged. Family text output
now composes the released shared renderer; actual GET storage export remains an
explicit injected writer integration rather than a successful no-op. See
[the compatibility and options matrix](../../docs/access-compatibility.md) and
[independent source fixtures](../../tests/compatibility/access/README.md).

`NamedResource`, `AclChange`, `LabelChange` and `MutationOptions` are the mutation
integration inputs. Inject an asynchronous `Confirmation`; force skips questions,
not validation/cancellation. The handlers preserve source role case, serialized
response substring update selection, assignment-ID removal and local label-name
matching. `CommandError` retains pre-failure diagnostics/warnings. Only fully
received HTTP 400 principal rejections advance the source probing order.

`FindOptions`/`find` implement command-line catalog search. `find_interactive`
uses injected `Confirmation` and `FindOutput` to emit each page before asking to
continue, including empty-page advancement and EOF/decline. SDK catalog helpers
remain separate from CLI type/support policy.

`ApiOptions`/`ApiInput`/`api` provide the raw status/header/text/query consumer;
`read_api_input` is a bounded real JSON/file input adapter. `build_api_request`
validates explicit origin, header and body policy before credentials.
Source HTTP errors remain raw data; transport/capability failures do not.
See [catalog/raw compatibility](../../docs/catalog-api-compatibility.md) and the
offline `composed_resources` example.

`acl_resource` adapts existing resolved context and checks source item exclusions.
`read_named_acls` adds OneLake security guidance without opening a browser or
altering security. `export_acls` writes source query/export intents to a required
`AclExportSink`, including empty/falsey results. The host's actual storage writer
remains an explicit integration requirement, not a default no-op. Handled OneLake
errors preserve exit 0 through `write_error`; the single front door must consume it.

`read_hidden_metadata` composes singleton/enriched reads, retaining original
internal metadata and source-suppressed diagnostic records alongside GET output.
`find_hidden_name`/`resolve_hidden_name` and `domain_name_by_id` preserve the
source case-sensitive first-match/name/default semantics over catalog rows;
they do not replace the separately owned context model. The actual HTTP adapter
now supports all four finite origin/audience pairs and safe custom headers.
Shared pager and generic operation reconciliation replace the earlier local
collector and incomplete 202 integration. Exact request/merged-ACL bytes use the
single released core encoder; no SDK-to-CLI dependency or second serializer exists.

`FamilySurface`, `FamilyPresentation`, `render_family_output` and
`render_family_error` connect the actual SDK/projection outcomes to shared text
or JSON output. ACL LS/local labels/FIND select source table headers, short FIND
selects its source truncation priorities, and GET/raw API keep ordinary routing.
The FIND blank following output and handled OneLake error guidance retain their
source stream order. Existing `CommandValue` literals and JSON writer APIs are
unchanged; no central host/parser/runtime or terminal/config discovery is added.
The caller supplies exact Unicode width/key/repr policy, presentation and writers.
Unsupported properties and I/O errors fail explicitly, retaining partial output.
