<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Additive definition source receipts

`definition_export_windows.json` and `definition_set_source.json` are new
synthetic fixtures, attributed under the repository MIT license. They preserve
the existing Python pin `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; no previous
fixture, expected output or oracle hash was changed.

Before observing native Rust results, the two reference scripts extracted and
executed actual function ASTs from that exact Git object:

- Export: `fab_cmd_export_utils.decode_payload`, `clean_notebook_cells`,
  `fab_storage._validate_json` and `_write_to_local`. Twelve cases include
  binary, empty/text, native newline, Unicode/number/null, suffix scalar and
  Notebook-output behavior.
- SET: `fab_cmd_set_utils._decode_payload`, `_encode_payload` and
  `ensure_notebook_dependency`. Six cases capture decoded edits and exact
  default JSON request bodies, including unchanged non-object/non-string
  edited payloads.

There is no product import, authentication, live HTTP, production Python bridge
or substituted implementation algorithm. The export receipt was captured on
Windows using only a synthetic temporary directory inside the caller's artifact
directory. The scripts refuse to overwrite an existing receipt.

To reproduce independently, use the existing development interpreter and new
absolute output paths, never the committed expectation paths:

```text
python definition_export_reference.py NEW_EXPORT_RECEIPT EXISTING_SYNTHETIC_TEMP_ROOT
python definition_set_reference.py NEW_SET_RECEIPT
```

Native non-Windows comparison must account for the Windows text-mode newline
translation in the export receipt, but must never transform binary payloads.
Source receipt capture is not a passing native comparison or full command
parity claim.

`definition_import_source.json` adds seven cases captured by
`definition_import_reference.py` before Rust import execution. It executes
the actual `_build_definition` and `_encode_file_to_base64` leaves on synthetic
directories: deterministic order, binary/CRLF preservation, empty directories,
Report byConnection/byPath/null, and Report validation before cache exclusion.
Definition format is an already-resolved low-level input; command preparation
independently resolves user selectors. Existing receipts remain unchanged.

`definition_bulk_source.json` comes from actual pinned bulk payload, workspace
filter and parent-prefix leaves with injected synthetic metadata/support
callbacks. It includes All/Selective request bytes, index matching and sibling
prefix filtering, plus four parent-prefix cases. No Rust response was used to
construct these expectations.
