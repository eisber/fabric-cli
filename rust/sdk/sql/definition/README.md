<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# SQLDatabase native definition codec

`fabric-sdk-sql-definition` is a pure SDK-layer content leaf. It admits and
serializes one immutable `NativeDefinition`, shared by SDK management consumers
and definition-only copy orchestration. It has no deploy, CLI, HTTP, identity,
runtime, filesystem or process dependency. There is no operation request wrapper,
approval policy, LRO engine, local writer or implicit credential selection here.

The additive `warehouse` module reuses multipart/Base64/path/primary admission
with Warehouse's **sqlproj** default. Its immutable `WarehouseDefinition`
retains the complete original response, native media (missing/null/exact string)
and unknown fields instead of applying SQLDatabase's strict unknown-field
profile. Existing SQLDatabase entry points/defaults/validation remain unchanged.
See [native Warehouse retrieval](../../../docs/warehouse-definitions.md).

## Local structural validation

`validation::register` adds the DACPAC validation operation and exact
`SQLDatabase/src/definition.dacpac` binding to the existing
`fabric-v2-contracts::Registry`. It does not create another schema registry or
engine. Registration is atomic and preserves existing entries on conflict.
The inert input/output schema projection carries rule version `1.0`, its source
revision and coverage limits; it is not a general JSON Schema evaluator.

`validation::validate` consumes original JSON bytes and delegates to the same
strict native codec above, with explicit media, format, rule version and all six
inclusive caller budgets. The first profile accepts a **bare** DACPAC definition
only. It does not unwrap a response, read a directory or execute/build a package.
The codec also supports SQL projects, but this validation profile deliberately
does not advertise them yet.

A completed report contains `operationId`, `ruleVersion`, `mediaType`,
`nativeFormat`, `valid`, `findings` and `coverage`. Malformed/duplicate envelope
JSON and invalid structure yield one error finding, with a safe RFC 6901 part
ordinal pointer where available. Empty pointer means root or unavailable finer
location; line/column and native filenames are not fabricated. Original native
Base64 strings and input bytes are not normalized or written.

Unsupported media, format, payload/part kinds or rule versions and exhausted
budgets return an error **without a validity verdict**. No network schema/ref
resolution occurs. The existing codec is fail-fast, so `findingPolicy` is
`first_failure`, not complete finding enumeration. Coverage is always explicitly
incomplete: native DACPAC/SQL semantics, decoded `.platform` JSON, permission,
service acceptance, reference existence, round-trip and runtime correctness
remain unestablished. Structurally valid opaque bytes need not be a valid DACPAC.

The raw wire-byte limit is enforced before format inspection. A unique string
`format` in a syntactically complete bare JSON object selects the profile before
part admission: explicit SQLproj or another unsupported format returns no verdict
even if its parts are empty, missing, malformed or would fail native admission.
Format inspection uses the existing JSON deserializer and does not parse native
payloads or evaluate a second schema. Duplicate/incorrectly typed format fields,
malformed JSON and trailing input retain the original strict codec's structural
findings; absent format retains the documented DACPAC default.
Format inspection is map-only: positional arrays such as `["sqlproj"]` do not
declare a document format and retain the codec's invalid-envelope report.

SDK callers can reuse this check before an explicitly selected create/update,
but local validation cannot grant approval, imply an upsert, replace native
service errors or establish an immutable checked-byte submission on their behalf.

## Public boundary

- `SqlDatabaseDefinitionFormat::{Dacpac, Sqlproj}` uses exact lowercase service
  spellings. Its only default is the documented native **dacpac** format.
- `NativeDefinitionPart { path, bytes }` is owned candidate content, not a local
  file. Constructors/parsers validate the complete definition before returning
  `NativeDefinition`. Its `format()` and `parts()` views are immutable.
- `sql_database_definition_from_parts` consumes explicit ordered parts.
- `parse_sql_database_definition` accepts a bare native definition; omitted
  authoring format defaults to dacpac.
- `parse_sql_database_definition_response` accepts `{ "definition": ... }` from
  getDefinition or its authoritative operation result. Omitted format inherits
  the requested effective format; explicit mismatch is rejected.
- `Serialize` emits the bare native definition with explicit effective format,
  original part order and unchanged admitted Base64 payload strings. There is
  no unchecked public constructor or `Deserialize` implementation.

Paths and bytes remain native: no text decoding, JSON formatting, newline/BOM
conversion, renaming, sorting, project interpretation or lossy string conversion.
`InlineBase64` is a wire encoding, not a MIME type. Decoded binary may contain
NUL, non-UTF8 or JSON-looking content. `.platform` is opaque bytes, not a
metadata/label schema validated by this package. Native/project/package validity
remains the service's responsibility.

## Caller-selected bounds

Every entry point requires explicit nonzero `DefinitionLimits`; there are no
numeric budgets or limit defaults. Limits are inclusive and independently cover
input JSON/compact bare-definition bytes, aggregate decoded bytes, per-part
decoded bytes, part count, UTF-8 native path bytes and path component depth.
Input wire size is checked before JSON deserialization. Complete UTF-8 and JSON
syntax are checked before content admission, so an early shape error cannot mask
invalid trailing content. This syntax-only pass builds no `Value`; admission
still reads the original input and rejects duplicate keys and unknown fields.
Part count is checked before allocating or decoding an excess native part.
Decoded sizes are checked before Base64 decoding. Checked arithmetic rejects
overflow.

Construction also counts compact serialized definition bytes without allocating
a second complete JSON body. Callers must bound larger enclosing requests,
responses, pretty serializers and local output independently. Parser input can
be smaller than its explicit-format serialization and still exceed the latter
bound. The caller's preallocated input is not retroactively memory-bounded.
Retaining both decoded bytes and original encoded payloads is a bounded-memory
tradeoff, not streaming or zero-copy.

## Intentional bounded admission profile

This is stricter v2 admission, not a silent change to legacy deploy behavior:

- Reject duplicate structural JSON keys, unknown fields, incorrect/missing/null
  fields, unsupported format/payload type and noncanonical standard Base64
  (including whitespace, URL-safe alphabet, missing padding and nonzero pad bits).
- Each native part must be a JSON object with string fields. Positional arrays
  are rejected, not normalized into objects; `payloadType` must be the literal
  string `InlineBase64`, not an array, object or other value.
- dacpac: exactly one nonempty lowercase `.dacpac` primary and optional root
  `.platform`. sqlproj: exactly one nonempty lowercase `.sqlproj` primary, zero
  or more `.sql` parts (including `.sharedqueries/*.sql`) and optional root
  `.platform`. Empty SQL files are permitted. Mixed formats, unrelated
  extensions and multiple primary parts are rejected.
- Do not require the literal basename `sqldb` or an example-derived root depth.
  The article names `sqldb.dacpac`/`sqldb.sqlproj`, while generated REST examples
  name `definition.dacpac`. Safe relative primary names and nesting are preserved.
  Arbitrary filenames/nesting and uppercase extensions have not been
  service-proven; accepting a name here does not guarantee service acceptance.
- Do not attest DACPAC ZIP/model integrity, project XML, SQL syntax, remote build
  semantics or backup fidelity. A DACPAC is a schema/model, not a data backup,
  BACPAC, complete settings/permissions/identity/label clone.
- Reject absolute/traversing/backslash/device/ADS/control/reserved/trailing-dot
  or trailing-space paths, exact duplicates, lowercase case aliases (including
  parent-directory aliases), and file/directory prefix conflicts. Do not trim
  leading slashes, normalize separators, resolve dot segments, percent-decode,
  rename paths or silently drop parts.

The portable path algorithm follows existing deploy path-policy prior art, but
cannot be imported through an SDK-to-deploy dependency. Legacy export additionally
formats text, trims leading slashes, permits exact-duplicate overwrite order and
uses an overwriting local adapter. None of those semantics are imported or
changed. There is no second JSON parser, Base64 engine or generic I/O policy:
serde/serde_json/base64 perform parsing/encoding; the small visitors enforce this
SQLDatabase wire shape and early part-count admission.

## Admission is not filesystem or operation authority

Portable path checks only admit **content**. Unicode lowercase comparison is not
proof of all native filesystem normalization, case, short-name, symlink/reparse
or concurrent-tree behavior. A separately authorized host adapter must provide
actual safe destination/no-clobber/race policy. Serializing or reading a
`NativeDefinition` cannot grant a local write capability.

HTTP status, permission denial and operation outcomes remain outside the codec.
Metadata-only, error and monitor bodies are not successful definition responses.
In particular, callers must preserve 401/403 failure and never substitute
metadata. Definition get needs native read/write and applicable encrypted-label
usage rights; labels are excluded from definitions, and definition update does
not change them. `.platform` does not implicitly copy a sensitivity label.

P1 owns management request wrappers and operation results over the existing
public Fabric operation engine. The coordinator owns definition-only copy's
per-step partial/unknown effects and separate same-workspace core folder move.
Copy does not imply rows/backup fidelity, transactionality, replay, cleanup or
source deletion. No upstream poller/ARM-origin/auth changes are adopted here.

## Evidence and validation status

Sources pinned in the contract-phase handoff:

- [SQLDatabase definition structure](https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/sql-database-definition),
  source `51e34c7286719812e28de4d44b0adeb84e062835`.
- [Get definition](https://learn.microsoft.com/en-us/rest/api/fabric/sqldatabase/items/get-sql-database-definition),
  source `3450951c8dc66a9ffd10b423b8aff5948da0006c`.
- [Update definition](https://learn.microsoft.com/en-us/rest/api/fabric/sqldatabase/items/update-sql-database-definition),
  source `b4d803a1e7f495553fcb7ef509391016e676d91d`.
- Deploy prior art at accepted repository base
  `eca98ab84c4a14d1ee158c2438e76dd77d2b0af0`:
  `rust\capabilities\deployment\fabric-deploy\src\definitions\codec.rs`,
  `mod.rs`, `local.rs` and independent `tests\definitions.rs`.

All fixture content is synthetic and embedded in tests; no sample payload is
represented as a service-validated DACPAC. Pure public tests cover native
roundtrips, structural/Base64/format errors, paths/collisions, all limit classes,
redaction and direct SDK consumption without a filesystem or deployment bridge.
No live Fabric/SQL/auth operation is needed or authorized.

**Source draft only:** builds, tests, formatting/lints/docs, architecture/impact
and combined-consumer checks have not run because no build allocation was
granted. Independent review and actual checks are still required. Shared
manifest/lock/default-member/facade/impact registration is coordinator-owned and
was intentionally not edited by this leaf.
