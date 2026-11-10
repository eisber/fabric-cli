<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Resource-command compatibility facade

`fabric-cli-resources` preserves the original aggregate package identity and all
public imports for ACL, label, FIND and raw API consumers. Its canonical handlers
now live in sibling CLI packages:

- `fabric-cli-access` at `../../commands/access`: ACL and label algorithms.
- `fabric-cli-catalog` at `../../commands/catalog`: FIND and hidden discovery.
- `fabric-cli-api` at `../../commands/api`: bounded raw API behavior.
- `fabric-cli-command-contracts` at `../../contracts`: the single shared
  error/value, confirmation, presentation and HTTP adapter definitions.

## Belongs / does not belong

Legacy reexports, `Registration`/`REGISTRATIONS`, original tests/examples, and
mixed argument/host composition belong here. Domain algorithms do not. New
capability packages never depend on this aggregate or on each other.

The physical `src/compatibility/` modules implement the original mixed parser
and host once. `pub use compatibility as frontdoor` preserves the old public
namespace, including nominal `AccessCommand`, `AccessKind`, `RawApiCommand`,
`AccessHost`, `ApiClients`, `AccessTargets` and `AccessOutput` definitions,
struct-literal fields and matchable enum variants. Root algorithm/type imports
are reexports, not duplicate definitions or wrappers with converted signatures.

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

## Provenance and verification

This extraction preserves the existing source algorithms, parsing, output/UI
ordering, tests and expected fixtures. The Python source baseline remains
`microsoft/fabric-cli@cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Independent oracle data remains under `rust/tests/compatibility/access/`,
`catalog/` and `raw_api/`; normal reference runs never rewrite it.

The entire former `commands/resources` package, including every test and example,
was first relocated here. The old and final package roots have equal depth, so
existing relative fixture paths resolve to the same `rust/tests/` files without
changing expected data or test bodies. The old `commands/resources` location is
reserved for the separately owned generic mutation package, not this facade.

`cargo test --offline --locked -p fabric-cli-resources` (only with an authorized
build slot) covers the original public paths plus extraction identity/direct
owner checks. Passing formatting/source checks alone is not compilation, test
registration, independent review, native-platform or live parity evidence.
