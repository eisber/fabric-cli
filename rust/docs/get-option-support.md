<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded GET option support

This slice follows Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, not the current Python checkout.
It changes only `--force` / `--verbose` handling for workspace GET and item
metadata-property queries. The existing no-option read behavior, SDK requests,
projection, JSON/text streams, cancellation and rendering are unchanged.
It is not full item GET, definition export, or live-service acceptance.

| Target/options | Implemented behavior |
| --- | --- |
| Workspace GET with `--force`, `--verbose`, or both | Existing workspace retrieval/enrichment and query, unchanged output |
| Item GET with a metadata-property query and either/both flags | Existing specialized metadata endpoint and query, without definition confirmation or sensitivity warning |
| Item GET without a metadata-property query and either flag | Explicit `NotSupported` after target discovery, before metadata/definition retrieval |
| Any `--output`, including a present empty value | Explicit `NotSupported` before discovery or file access; never silently ignored |
| No options | Existing read path, including its previously documented gaps |

The source metadata check is an exact, case-sensitive root split at the first
dot. Recognized roots are `id`, `type`, `displayName`, `description`,
`workspaceId`, `folderId`, and `properties`. For example, `properties.nested`
is metadata; `properties[0]`, `Properties`, `.`, and an absent/empty query are
not. This is intentionally not an AST-based JMESPath classifier.

The pinned parser describes `--verbose` as showing all JSON properties, but its
value is never consumed by the GET dispatch, handlers, query helper or error UI.
The handlers' separate `verbose=True` parameter controls printing, and
`deep_traversal` controls path inventory. Neither comes from `args.verbose`.
The bounded host preserves that source behavior rather than inventing deeper
traversal or verbose service-error disclosure. `--force` is similarly inert for
workspace and metadata-only item GET, where no definition prompt occurs.

## Integration and remaining branches

`fabric_cli::session::get_options::is_metadata_property_query` exposes the source
classifier. Both `ReadSession` and direct `ReadHost` enforce the same option
policy. The native executable shares `preflight` for early option checks and
defers context-sensitive flags to that target validation; it does not reject
force or verbose before the session can apply the metadata policy.
The original unconfigured/default-host contract remains unchanged.

Full item definition GET still requires the source decoded-value codec, including
`payloadType`, per-part errors, nested parts and Notebook output cleaning, plus
the complete existing item enrichment flow. `fabric_deploy::definitions`
currently exposes raw definition reads and export file bytes, not this decoded
GET value. Its `read_item_definition` also uses generic metadata, while GET
requires the specialized endpoint. Do not substitute export/SET decoding or a
second operation loop. Reuse specialized metadata plus the existing
`read_definition` / shared operation session when the original codec owner
provides an additive GET-value interface.

GET `--output` writes the **projected result**, even when empty, not definition
parts and not an output envelope. The pinned storage path is
`<output>/<full_name>/get_<local timestamp>`; local JSON receives `.json` twice
through `do_output` and `_write_to_local`. Local output uses pretty Unicode JSON
and native newlines; Lakehouse output uses default ASCII JSON. Preserve these
source oddities unless the owner separately approves a change.
Closing this branch needs the original storage encoder exported for projected
values and an explicit caller-owned local timestamp/file host. Existing
`TransferFiles` supplies async path/preflight/write policy, but the definition
parts decoder is not a raw result serializer.

Neither missing interface authorizes a duplicate serializer, generic endpoint
substitution, implicit consent, ambient clock, new credentials, or live writes.
Controller/original-library-owner integration must also cover prompt decline,
EOF/error, cancellation, partial file/service effects, and failed output delivery.

## Offline evidence

From `rust/`, the isolated source helper is:

```text
python -I -B hosts\cli\fabric-cli\tests\test_get_options_source.py
```

It executes selected unchanged functions from the exact Git object with explicit
offline dependencies, never imports the product, and reports pinned versus
current-checkout SHA-256 values separately. It observes metadata classification,
confirmation bypass, decline/EOF/force behavior, inert verbose handling,
query/output precedence, writer failures and the local double suffix.
It does not replace or regenerate protected fixtures or baselines.

The new Rust integration target is `fabric-cli --test get_options`; it denies
`unnameable_test_items` and covers the real session/direct host, byte-for-byte
option/no-option output, SDK requests, negative definition/output combinations,
invalid format, cancellation, service errors and failed writers.
Rust compilation/execution and independent review remain controller validation
requirements, not evidence supplied by formatting or the Python source helper.
