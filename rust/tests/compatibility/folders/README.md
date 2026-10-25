<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent folder LIST/GET source oracle

`cases.json` is new, hand-authored synthetic evidence. It was not generated from
Rust or copied over existing workspace/item/capacity/HTTP expectations.
`reference.py` verifies full LF-normalized SHA-256 digests of source files at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, then executes the actual AST bodies
of `list_folders`, `get_folder`, `do_request`, `_handle_successful_response`,
`ApiResponse`, exception classes and error helpers. It does not reimplement the
request merge or paging algorithm. Python is a development-only oracle.

It reuses the existing item oracle's AST compiler, case-insensitive test headers,
strict JSON comparison and I/O guard only. No item cases, observation function
or expectations are changed. Auth, session, time, context and logging are fake.
Every HTTP call must use GET, the exact expected URL, Fabric scope and JSON
content header. Guarded execution denies file access/writes, socket/DNS,
processes, sleeping, real imports and ambient configuration. Only the injected
auth/context modules and CPython's already-loaded `json.decoder` error module
can be returned by the import guard; no import loader runs. An attempted
escape remains a failure even if product code catches the exception.
`test_reference.py` exercises source drift, injected AST escape attempts,
URL/row/error comparison mutations and actual pure memstore extraction helpers.

Run from `rust/` only within the authorized shared-host gate:

```text
python tests/compatibility/folders/reference.py --check
python -m unittest discover -s tests/compatibility/folders -p test_reference.py
cargo test --offline --locked -p fabric-sdk-folders
cargo test --offline --locked -p fabric-sdk --test folder_facade
cargo test --offline --locked -p fabric-sdk-http --test folder_adapter
```

## Denominators

There are **26 comparable raw SDK cases** for exact routes, query ordering/
replacement/encoding, recursive presence/None/False/string case, root attribute
override, no-request-dictionary memstore usage, token/header precedence, ordered
duplicate/future/null rows and error-code precedence. The Rust standalone
consumer checks these literal expectations; Python independently produces the
same URLs, rows and error codes. Python's envelope/`total_pages` output is not
the Rust `FolderList` progress contract. A later-page error exposes no rows in
Python; Rust separately tests explicit committed partial progress.

There are **13 raw-Python quirk cases**, not counted as parity successes.
Twelve have corresponding explicit Rust safety exceptions: missing value,
null/malformed/empty payloads, null required ID, numeric continuation token,
cross-origin active URI, discarded follow-up 201, object GET pagination,
204, 202 with wait disabled and recursive URI split errors. The thirteenth
documents Python's `AttributeError` when `folder_id` exists without a request
dictionary; Rust's explicit options always supply that dictionary, so there
is no equivalent invalid state. The source response bodies or exception names
are literal expectations, not auto-recorded outputs.

Additional Rust safety tests cover real default exact/+1 100 pages, 100,000
rows and 4 MiB, page-atomic progress, initial/cyclic tokens, active URI matrix,
encoded IDs, pre-auth path/query/audience errors, provider failure, in-flight/
between-pages/ready cancellation and drop. Public facade and external HTTP
consumer tests exercise shared execution, retries, deadlines, response limits,
service partial failure and stream cancellation. These tests do not expand
the raw-Python parity denominator.

## Actual consumers inspected, not ported

Full file digests also pin memstore, folder hierarchy, context resolution,
`fab_cmd_fs_utils`, folder `ls` and folder `get` handlers. The actual pure
memstore extraction helpers are additionally executed against synthetic parent
objects. Tree assembly, path and CLI handlers remain inspected/hash evidence,
not executed CLI parity claims:

* `fab_mem_store._get_workspace_folders_from_api` passes a namespace with only
  `ws_id`. It builds roots only when `parentFolderId` is absent, then repeatedly
  resolves nested folders. Orphans/null parents can fail to make progress.
  The SDK preserves those fields/rows rather than importing that loop.
* `get_folder_id`/`get_nested_folder_id`, context path resolution and `Folder`
  reconstruct names/parents with opaque IDs; no source UUID restriction exists.
* `get_ws_elements` filters memstore items/folders by the selected parent,
  controlled by folder-listing configuration. Sorting/output occurs later.
* The actual folder GET handler sets `ws_id`/`folder_id`, parses the object and
  delegates query/export to CLI helpers. The SDK stops before those CLI steps.

No live call, auth backend, transport/TLS choice, CLI composition or full folder
mutation parity is established by this evidence.
