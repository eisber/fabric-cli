<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Workspace-list validation matrix

This independent reference targets Python source
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, adopted through Rust foundation
`eccfca1d3b1cb4df71d0fa65a76221a588095290`.
[Harness and immutable fixtures](../tests/compatibility/workspaces/README.md)
are development-only. Existing Python tests and expected outputs are unchanged.

| Contract | Python source path under `src/fabric_cli` | Independent case | Rust gate |
| --- | --- | --- | --- |
| GET `https://api.fabric.microsoft.com/v1/workspaces`, Fabric default scope | `client/fab_api_workspace.py::list_workspaces`, `client/fab_api_client.py::do_request` | All eight | Await agreed injection interface and adapter. |
| Empty array remains `result.data: []` | `utils/fab_mem_store.py::_get_workspaces_from_api`, `core/fab_output.py::OutputResult` | `empty` | Not demonstrated. |
| Service order vs stable case-insensitive suffixed CLI order; unknown types ignored | `core/hiearchy/fab_workspace.py::Workspace`, `core/fab_types.py::WorkspaceType`, `commands/fs/ls/fab_fs_ls_workspace.py::exec`, `utils/fab_cmd_ls_utils.py::sort_elements` | `names_types_order` | Compare raw SDK rows separately from CLI translation. |
| Recursive token traversal, token encoding, ignored URI, empty intermediate page, observed `total_pages: 1` | `client/fab_api_client.py::_handle_successful_response`, `client/fab_api_types.py::ApiResponse.append_text` | `token_pagination` | Rust bounds/origin checks are deliberate safety requirements, not observed Python parity. |
| JMESPath filtering and projection on name/id rows | `utils/fab_cmd_ls_utils.py::format_and_print_output`, `utils/fab_jmespath.py::search` | `query_projection` | CLI behavior; do not add SDK-side invented query semantics. |
| Missing value: `KeyError` to `UnexpectedError`, exit 1 | `utils/fab_mem_store.py::_get_workspaces_from_api`, `main.py::_handle_unexpected_error` | `missing_value` | Typed Rust malformed-response behavior requires explicit mapping, not success-shaped empty results. |
| 403 standard message and header-derived error code | `client/fab_api_client.py::do_request`, `errors/common.py::CommonErrors.forbidden` | `forbidden_mapping` | Not demonstrated. |
| Fabric body error and request ID; JSON omits details/request ID | `core/fab_exceptions.py::FabricAPIError`, `utils/fab_ui.py::print_output_error` | `fabric_error` | Not demonstrated; scaffold JSON errors do not count. |
| Root resolution despite workspace context, hidden collections, context unchanged | `core/fab_handle_context.py::get_command_context`, `core/fab_context.py::Context`, `commands/fs/ls/fab_fs_ls_workspace.py::exec` | `root_from_workspace` | CLI/context behavior, not the raw SDK list. |
| JSON envelope/streams/exit | `core/fab_output.py`, `utils/fab_ui.py::_print_error_format_json`, `core/fab_decorators.py::handle_exceptions`, `main.py` | All eight | Python JSON errors go to stdout with empty stderr; separate from Rust bootstrap contracts. |

## Exception and exit mapping

Successful workspace helper return `None` maps to 0, as in Python `main`.
`handle_exceptions` renders `FabricCLIError` subclasses through the real output
helper and returns 1, except the source's `Unauthorized` code maps to 4 (not
one of these eight cases). HTTP 403 is mapped before Fabric body decoding.
Other Fabric HTTP failures, such as the synthetic 400, use `FabricAPIError`.
Missing `value` escapes that decorator as `KeyError`; the real main unexpected-
error helper renders `UnexpectedError` and exits 1 without a command field.
JSON error output goes to stdout, not stderr. This records source quirks without
changing them or asserting all possible errors have been covered.

## Evidence and limits

Implementation is present; execution evidence is pending a named parent grant
and an available isolated Python interpreter. No test, full build, restore,
Rust adapter, live API, authentication, native-platform parity, or Rust result
is claimed by this document.

The reference checks exact deterministic stdout, stderr, exits, structured
data, exception class/code/request ID, request order, credential scope sequence,
and unchanged in-memory context. Five side-effect probes and ten observation
mutations check that its safeguards/comparator actually reject failure. The
aggregate source hash fails closed on Python drift. There is no snapshot update
mode or waiver/skip path.

Out of scope: full parser/dispatch invocation, text/table layouts, `--long`
capacity enrichment, cache TTL/invalidation, persisted context, retries/429,
transport exceptions, authentication, Rust safety-bound tests, and live service
schema confirmation. This is not a coverage measurement; the coverage lane owns
existing-suite line/branch evidence.
