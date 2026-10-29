<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# GET definition values

`fabric_deploy::definitions::decode_get_definition_value` translates the pure
GET definition-value flow from Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. It does not acquire metadata,
export files, run the GET command, or claim complete command/release parity.

## Calling the API

Pass the **actual specialized endpoint metadata**, the acquired definition
response dictionary, the exact source item type, an explicit
`GetDefinitionPreprocess` selector and `DefinitionLimits`.

The function applies source `dict.update` semantics before preprocessing.
Existing metadata keys retain their positions; definition properties overwrite
their values. A `None` definition is an explicit caller-owned metadata-only
choice, including an already-handled 401/403 fallback, not an ignored acquisition
failure. `Preserve` models `decode=False`; it does not clean Notebook outputs.

The `GetDefinitionValue` tree retains ordered dictionaries/lists, native Python
scalar values and binary bytes. `try_to_python` preserves wide integers,
nonfinite floats and lone-surrogate strings, but rejects bytes.
`try_to_json` additionally rejects values outside finite serde JSON rather than
rounding or stringifying them. Both conversions report typed failures with a
logical location. Bytes require the caller's explicit rendering/projection
policy; they are not an invented JSON wrapper or base64 replacement.

The API is frontend-neutral. It performs no network, filesystem, runtime, clock,
prompt, configuration, credential or SDK-provider operations.

## Source behavior

The unchanged source functions executed by the offline oracle are
`get_item_withdefinition`, `export_single_item`, `resolve_definition_format`,
`decode_payload` and `clean_notebook_cells`, plus the actual source item-type and
format declarations. External acquisition is replaced only by explicit
synthetic response providers.

| Branch | Value behavior |
| --- | --- |
| Part selection | Prefer `definition.parts` when present; otherwise use `definitionParts`; absent parts are valid |
| Order/path | Keep service order, duplicate paths, leading slashes and exact path spelling; no filesystem path normalization |
| Payload presence | Missing and Python-false payloads remain unchanged |
| Payload type | Ignore the incoming type when deciding to decode; successful decoding sets `payloadType` to `DecodeBase64` |
| UTF-8 | Valid UTF-8 becomes text; invalid UTF-8 becomes actual bytes, even with a JSON suffix |
| JSON suffix | Only exact `.json`, `.ipynb`, `.pbir`, `.platform`, `.pbism` suffixes invoke Python-compatible JSON parsing |
| JSON values | Preserve scalar versus text, duplicate-key first position/last value, wide integers, nonfinite floats, signed float zero and Unicode identities |
| Caught failure | Keep original payload/type and replace/add `error` with the source decoding error; continue later parts |
| Successful decode | Preserve an already-existing `error` property, as the source does |
| Nested marker | Presence of `nested_parts` recurses into that part's `definition.parts`/`definitionParts`; it does **not** traverse the marker's array |
| Notebook | Only exact `Notebook`, only the first `definition.parts` payload, only existing `outputs` fields; keep other parts and execution counts |

Notebook cleanup also preserves source membership/iteration behavior for JSON
strings, lists and dictionary keys. Invalid operations such as a binary/scalar
Notebook payload or assigning `outputs` through a string/list produce a typed
structural error, not empty success.

There are no Report, Environment or SemanticModel-specific branches in the
source decoder. Their actual format and content differences pass through the
same algorithm. Source contracts include Report PBIR, SemanticModel TMDL/BIM,
Environment YAML, DataPipeline, SparkJobDefinition, VariableLibrary,
MirroredDatabase and Notebook.

## Explicit bounds and domain exceptions

`GetDefinitionValueError` carries an existing `DefinitionError` classification
and a logical JSON-pointer location. It never uses completed/incomplete file
paths to pretend decoding performed I/O. Malformed dictionary/part structures
are `InvalidDefinition`; no partially decoded value is returned on such errors.

Canonical standard base64 is decoded. ASCII unpadded alphabet-only inputs that
Python rejects preserve their source per-part padding error. Non-ASCII inputs
and truthy non-string JSON payloads preserve their source per-part type error.
Other noncanonical/permissive encodings, including ignored punctuation,
whitespace, nonzero padding bits and excess/incomplete padding, explicitly fail
with `Unsupported` at the payload. This is the existing strict definition codec
policy, **not** a claim that Python rejects these inputs. The oracle records
Python's actual accepted values/errors separately.

`DefinitionLimits.max_bytes` conservatively charges the combined inputs,
decoded payload bytes and final result: input strings use UTF-8 length; result
strings/keys use four units per Python codepoint; nodes use one unit.
`max_files` and `max_depth` bound the parts visited by decoding;
`max_path_bytes` bounds decoded text path examination. The shared native JSON
node, depth, string and 4,300-decimal-digit bounds also apply. Bounds fail
explicitly with `Limit`, never become source per-part fallback or truncated
success. No new dependency or numeric/Unicode waiver is introduced.

JSON error text in the source receipt was observed with CPython **3.13.15**,
including its explicit illegal-trailing-comma diagnostics. The parser shares
the existing export JSON implementation and adds source error locations; export
serialization/import bytes and the public export error policy are unchanged.
Different Python patch versions can word standard-library errors differently;
this receipt does not assert those alternate diagnostics.

## Host integration still required

The caller still owns command-support checks, consent, format resolution, the
specialized metadata endpoint, the existing shared `read_definition` operation
session and explicit access-denied handling. After decoding it owns
connections/schedules and Environment/mirroring enrichments, projection,
rendering and projected-result `--output` delivery. Those are not replaced with
generic metadata, another operation loop, file-export values or a success flag.

This slice closes the pure decoded-value seam described in
[GET option support](get-option-support.md); it does not itself wire that seam
into the host or establish all item/command leaves or production GA.

## Offline validation

From `rust/`, verify the newly added source receipt without replacing it:

```text
python -I -B capabilities\deployment\fabric-deploy\tests\get_definition_value_reference.py
```

The helper executes complete selected functions directly from the pinned Git
object with standard-library-only explicit dependencies. It compares 130
synthetic cases, includes independent fixed assertions, and records pinned and
checkout SHA-256 values separately. It imports no product modules and changes
no existing frozen oracle, baseline or expectation.

The Rust target denies `unnameable_test_items` and includes separate independent
assertions and bounds/conversion failures. The serialized validation owner must
run the following selectors before integration; source execution/formatting
alone does not establish Rust compilation, registration or execution:

```text
cargo test --offline --locked -p fabric-deploy --test get_definition_value
cargo test --offline --locked -p fabric-deploy --test export_parity --test definitions
cargo check --offline --locked -p fabric-deploy --all-targets
cargo test --offline --locked -p fabric-deploy --doc
```
