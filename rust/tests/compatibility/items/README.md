<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent offline Python item LIST/GET evidence

This directory is a development-only Python compatibility oracle for the generic
`/workspaces/{workspace_id}/items` LIST and
`/workspaces/{workspace_id}/items/{item_id}` GET boundaries. It is not a Rust
implementation, production Python bridge, complete CLI test, or live recording.
All inputs and expected outputs are hand-authored synthetic data. No expectation
is obtained from Rust, and there is no record/update/regenerate mode.
Existing workspace/HTTP fixtures and Python production source are untouched.

**Authoring validation status:** no reference tests, Cargo, compilers, builds,
live calls, authentication, or heavyweight processes were run by this lane.
The shared host's reserved build slot was not acquired. Execution below is for
the parent after acquiring the host. Source files were read, digested, and
checked unchanged against the baseline with a read-only Git diff.

**Parent execution:** all 56 original cases and seven guard tests subsequently
passed on Windows x64/Python 3.13.15 in the assigned item SDK host slot. The
additive `query-encoding.json` is a separate, hand-authored punctuation golden
executed by `test_reference.py` and Rust; the original 30+26 cases remain unchanged.
See the [measured item matrix](../../../docs/item-compatibility.md).

## Exact source provenance

Baseline: `microsoft/fabric-cli`
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.

The harness checks the complete SHA-256 of each executed source file before
parsing its AST. Only checkout CRLF is normalized to LF; no other whitespace,
encoding, or content normalization is allowed. Paths below are relative to
`src\fabric_cli`. Source drift fails closed, not by accepting new outputs.

| Executed source file | SHA-256 (LF-normalized UTF-8) |
| --- | --- |
| `client\fab_api_workspace.py` | `c2512eec29b12b60a29e013ff0a296833f86e59904c76418ef406e903eb49ab8` |
| `client\fab_api_item.py` | `d0f257eb327f759bbdc468a8f9be1af4cce0116e7f4ee6f8e2536c10ca36d740` |
| `client\fab_api_client.py` | `d7d978f315921ec4559a2fcefc992435152758b74470d1d969006def088e8e0d` |
| `client\fab_api_types.py` | `990708f777081bb07fdd80906bcd4e49f22f4f1af6e7cd522808a89ff3b34c40` |
| `core\fab_exceptions.py` | `85826f456cc9f2bfd026950fc0338b04570cf56c571513f1cef1feac386db0dc` |
| `core\fab_constant.py` | `20dd87282310f0217b3f3724214c280c3dab123bec5f7fafa770e1c2c44bfff0` |
| `errors\common.py` | `f0af9183ba94b1ab89f6bd7afe1bca71538704afd5570e73b913fd45b2c1154d` |

The following consumer files were inspected for the boundary definition. These
are documentary source pins, **not executed CLI coverage** or part of the runtime
seven-file digest guard.

| Inspected consumer | SHA-256 (same normalization) |
| --- | --- |
| `utils\fab_mem_store.py` | `5f1bb375ca2e1da68c454de622b1f624f8187e73c8ef8db317f553bad9f12620` |
| `utils\fab_item_util.py` | `99a5045c607e20bfa4a126f5342cb0e29e5357343a85a3db177ec32d80ac4983` |
| `utils\fab_cmd_get_utils.py` | `59e8f6de44b997d8f4322de63e4ebd9d1c6aca87fe302c7948b26faa4f8b0e4a` |
| `utils\fab_cmd_fs_utils.py` | `9743af42473a6d54bf9feae7e8775fa3452183ec3502a8b78755cbccd84f52f1` |
| `commands\fs\ls\fab_fs_ls_item.py` | `e730e995d225e3e7bf57536dc724c21e7c501e6222b34d9a47cee0d91f3b8915` |
| `commands\fs\get\fab_fs_get_item.py` | `63680f3ff138dcc93db2575a8d4602957aefd9c2dc15bc168da6c974a32057ac` |

The repository MIT license applies to the Microsoft source and all new files.
The JSON file carries its copyright/license notice in `_copyright`.
Unicode fixtures use ASCII JSON escapes to cover composed/decomposed names,
astral characters and non-ASCII continuation/query values without normalization.

## Real source boundary, fake external dependencies

`reference.py` uses only the Python standard library. It extracts and executes
the actual AST definitions of:

1. `ls_workspace_items` and `get_item`.
2. `do_request`, `_handle_successful_response`, and
   `_transform_workspace_url_for_private_link_if_needed`.
3. The full `ApiResponse` class, including `append_text`.
4. The actual exception classes, error defaults, and `CommonErrors`.
5. The selected actual endpoint/version/scope/error/debug/config constants.

No item URL construction, pagination algorithm, list merge, or service error
parser is rewritten in the harness. Source imports are not executed. Each
observation gets a fresh namespace, arguments, injected auth/context modules,
configuration accessor, fake logger/clock, and scripted session. This follows
the conceptual dependency-injection prior art in
`..\workspaces\reference.py`, without importing or modifying that oracle.

The injected auth only returns a synthetic sentinel or raises the scripted
`FabricCLIError`; scopes and acquisition count are checked. The source still
constructs the authorization header, content type, timeout, method and URL.
The session checks those arguments and consumes every response exactly once.
Additional requests and unused responses fail. The context contains only the
synthetic command and no Fabric skill. Private links/debug are disabled, endpoint
environment validation returns the source default, and user-agent construction
is replaced with a fixed sentinel to avoid platform/environment inspection.

The fake requests module exposes the same stdlib `urllib.parse.urlencode` used
by `requests.compat.urlencode`, an exception type, and a minimal case-insensitive
response-header mapping. Responses serialize the supplied JSON body with Python
`json.dumps`. The harness does **not** prepare/send a real requests request;
observed URLs are exactly the strings passed to `Session.request`.

During source execution, only the two injected auth/context imports are allowed.
A process-local audit hook and patched boundaries deny file reads/writes,
directory access/mutation, sockets/DNS, subprocesses and sleeping. A caught
violation still fails the observation. No temporary directories, home/config
setup, product imports, ambient credentials, network, or third-party packages
are needed. Reading fixtures and verifying/parsing source occurs **before**
entering that no-I/O boundary. These are regression guards, not an OS sandbox
against hostile native code or arbitrary interpreter startup hooks.

## Comparison schema

`cases.json` has top-level `source_commit`, `cases` (30 comparable cases), and
`python_only` (26 explicitly non-comparable source quirks).

Each comparable case has:

```text
id, operation ("list" | "get"), workspace_id, optional item_id,
optional query: [[name, value], ...],
responses: [{status, headers, body: any JSON value}, ...],
optional auth_error: error-code string,
expected: {
  requests: [{method: "GET", url, scope: "https://api.fabric.microsoft.com/.default"}],
  rows: [unaltered JSON item, ...] OR error_code: string
},
python_expected: {status_code, body} OR {exception_type, error_code, message, request_id}
```

`GET` has one row containing its whole successful object; LIST rows are the
merged `value` array in service order. The harness uppercases the source's
lowercase HTTP method only at the observation seam. It does not sort, deduplicate,
filter types, project fields, fill missing fields, normalize names, or drop
unknown/null data. Rows and URLs must compare exactly. Core rows deliberately
have string `id`/`displayName`, but **do not require `type`**. Missing, null, and
future type values and flattened extra JSON must survive Rust comparison.
The comparator is scalar-type-sensitive: Python's `True == 1 == 1.0` must not
hide JSON type drift. Object key order is immaterial; array order is not.

The ordered query pairs create Python's insertion-ordered `args.request_params`
dictionary; comparable cases use unique string keys and string values.
`type=Notebook` is a server query parameter, not a client-side predicate: a
synthetic mismatched server row remains in the result. Neither helper has a
separate built-in `args.type` filter.

`python_expected` independently records the complete parsed raw success body,
including pagination/envelope metadata, or the actual source exception type,
code, stripped message and request ID. This is separate from the SDK's rows/code
comparison; it is not CLI stdout. Observations additionally expose
`auth_scopes`, including the acquisition on an auth failure before any request.

Python-only cases use the same inputs, plus `reason` and optional `item_uri` /
`ext_uri` strings. Their `expected` checks only the request trace;
`python_expected` checks the full raw success or native exception. This avoids
inventing an SDK error code or rows shape for behavior the source does not have.
Do not silently include these in Rust success comparisons.

## Comparable case inventory

| Case ID | Contract |
| --- | --- |
| `list_empty` | Empty collection; no invented `total_pages`. |
| `list_order_duplicates_unicode_optional_type` | Exact order, duplicate rows, Unicode, future/missing/null type, extra/nested/null fields. |
| `list_three_pages_empty_middle` | Recursive three-page traversal; opaque tokens; empty middle page; duplicate preservation; all original list-valued keys merge. |
| `list_type_query_encoding_not_local_filter` | Type query forwarded; spaces/plus/slash/equals/Unicode encoded; no local filtering. |
| `list_query_retained_opaque_token` | Existing query retained while opaque continuation token is encoded. |
| `list_header_token_precedence_case_insensitive` | Header token wins over body token, case-insensitively. |
| `list_empty_header_suppresses_body_token` | Presence of an empty header suppresses body fallback and traversal. |
| `list_empty_body_token` | Empty string stops; raw token field remains. |
| `list_null_body_token` | Null stops; raw token field remains. |
| `list_continuation_uri_alone_ignored` | `continuationUri` does not initiate traversal. |
| `get_generic_preserves_nested_extra` | Generic GET, nested and extra data preserved. |
| `get_missing_type` | No synthetic required type/default. |
| `get_null_type` | Explicit null type differs from absent type. |
| `get_future_type_unicode` | Future type, astral/decomposed names, unknown nested data. |
| `get_request_params_query_encoding` | GET request parameters and empty query value encoding. |
| `list_403_public_header_precedence` | Public header beats secondary header/body; standard message. |
| `get_403_secondary_header` | Secondary header used when public header is absent. |
| `get_403_default_ignores_body` | Default `Forbidden`; body code/message ignored. |
| `list_401_public_header` | Same public-header precedence with unauthorized message. |
| `get_404_ignores_headers_and_body` | Always `NotFound`; standard message. |
| `list_400_body_over_headers` | Fabric body code/message/request ID; all trailing periods stripped. |
| `get_500_body_error` | Fabric body error mapping outside the special statuses. |
| `get_400_missing_body_code` | Missing top-level code/message becomes source `UnknownError`/default message, not a nested error or header code. |
| `list_auth_failure_before_request` | Auth failure produces no request. |
| `get_auth_failure_before_request` | Same for GET. |
| `list_page_two_error_no_partial_success` | A later error propagates; no success/partial rows. |
| `list_403_empty_public_falls_back_secondary` | Error-header fallback uses truthiness, unlike continuation-header presence. |
| `get_401_default` | Default `Unauthorized` code, ignoring body. |
| `list_single_page_metadata_not_rewritten` | Single-page envelope metadata stays untouched. |
| `list_query_initial_continuation_replaced` | Returned token replaces an initial request token without moving its query position. |

## Source quirks and deliberate non-comparability

`do_request` splits the raw URI at `?`, uses only segment 1 for the query,
splits pairs on every `=`, and never URL-decodes them. Existing request params
are overwritten in place by raw URI pairs; the resulting dictionary is encoded
again. Path components are interpolated literally. `get_item(item_uri=True)`
substitutes the specialized collection; `ext_uri=True` appends its string
without adding any separator.

| Python-only case ID | Why separate |
| --- | --- |
| `python_only_get_item_uri_ext_uri` | Specialized environment/staging libraries path; repeated raw query key is last-wins. |
| `python_only_get_ext_uri_no_separator` | Literal suffix concatenation, no separator insertion. |
| `python_only_get_dot_item_id` | Source forwards `.`; Rust rejects it as an explicit safety exception. |
| `python_only_get_dotdot_workspace` | Source forwards `..`; Rust rejects traversal. |
| `python_only_get_separators` | Source forwards slash/backslash identifiers; Rust rejects separators. |
| `python_only_get_raw_space_unicode_fragment` | Unescaped raw path/fragment at the session boundary, not prepared wire URL. |
| `python_only_get_raw_query_override_double_encoding` | URI query overrides supplied params; `+` and `%2F` are re-encoded literally. |
| `python_only_get_multiple_question_marks` | Extra question-mark segments silently disappear. |
| `python_only_get_query_missing_equals` | Native `ValueError`, before auth/request. |
| `python_only_get_query_extra_equals` | Native `ValueError`, before auth/request. |
| `python_only_get_query_trailing_ampersand` | Empty pair raises native `ValueError`, before auth/request. |
| `python_only_list_missing_value` | Successful raw object, not source-side `KeyError` at this boundary. |
| `python_only_list_null_value` | Successful null collection at API boundary; typed Rust rejects. |
| `python_only_list_object_value` | Successful non-array collection at API boundary; typed Rust rejects. |
| `python_only_list_array_token_membership` | Token substring/membership reaches string indexing on a list: native `TypeError`. |
| `python_only_get_null_body` | Successful null, not a validated item. |
| `python_only_get_missing_required_fields` | Successful `{}`; Rust requires string id/displayName. |
| `python_only_list_invalid_item_shapes` | Invalid/missing/null/numeric required fields and scalar/null rows preserved by Python. |
| `python_only_list_next_201_not_merged` | Followed page with status 201 is discarded, not merged. |
| `python_only_list_append_null_next_value` | Extending with null raises native `TypeError`. |
| `python_only_list_append_mapping_keys` | Extending with an object appends its keys, not item objects. |
| `python_only_list_numeric_token` | Numeric token accepted/stringified through Python truthiness. |
| `python_only_list_false_token` | Boolean false token accepted as stopping condition. |
| `python_only_get_paginates_any_object` | Generic GET object can trigger paging/list-field aggregation. |
| `python_only_get_error_body_array` | Fabric error parser assumes an object: native `AttributeError`. |
| `python_only_list_204_null` | Accepted 204 with JSON null, not an empty-wire-body fixture. |

Recursive merges keep first-page non-list fields, ignore new later-page keys,
remove `continuationToken`/`continuationUri`, and set **`total_pages: 1` even for
three pages**, because every append uses the default `total_pages=0`. A
single-page result has no invented metadata or token removal. Only a followed
response with status exactly 200 is merged. The original `ApiResponse.content`
and headers are not updated by `append_text`; this harness compares parsed
`text`, not those unchanged wire fields.

These observations are not permission to remove Rust's required bounded
pagination, origin/deadline/cancellation checks, explicit incomplete-result
errors, typed required-field validation, or path safety policy. Keep such
exceptions separately asserted rather than changing these expectations.

## CLI consumers and coverage limits

`fab_mem_store._get_workspace_items_from_api` calls `ls_workspace_items`, indexes
`data["value"]`, and constructs `Item` objects with `displayName`, `id`, folder
resolution and `type`. It suppresses only `FabricCLIError(InvalidItemType)`;
unknown types can disappear there, while missing keys/null/malformed data may
fail later. `fab_cmd_fs_utils.get_ws_elements` and `sort_ws_elements`, then
`fab_fs_ls_item.exec`, apply folder selection, sorting and display projection.
Those consumer effects are deliberately **not** applied to raw SDK rows.

For GET, `fab_item_util.get_item_with_definition` uses
`format_mapping.get(item.item_type, "items")`, and calls `get_item(item_uri=True)`
for metadata-only queries or an unsupported-export fallback.
`fab_fs_get_item.exec` can also obtain definitions, connections, schedules,
environment metadata and mirroring status before querying/exporting output.
`fab_cmd_get_utils.get_environment_metadata` uses the specialized `item_uri`
and `ext_uri` forms. These are inspected consumers, not claims of parity for
CLI rendering, metadata aggregation, definitions or mutation/export operations.

Other gaps are intentionally explicit: no real requests URL preparation,
transport retry/connection failures, malformed JSON bytes/empty wire bodies
(the fixture body is a JSON value), rate-limit sleeps, LRO polling, private
links, endpoint overrides, request-header overrides, non-Fabric audiences,
real authentication/config/cache/terminal output, or unbounded-token/cycle
execution. Existing HTTP/workspace evidence remains independent.

## Deferred execution

After the parent acquires the named shared-host slot, from the repository root
with a trusted Python 3.10+ interpreter (no third-party installation required):

```powershell
& <python.exe> -I -B rust\tests\compatibility\items\reference.py --check
& <python.exe> -I -B rust\tests\compatibility\items\reference.py --check --include-python-only
& <python.exe> -I -B rust\tests\compatibility\items\test_reference.py -v
```

Without `--check`, the runner still checks expectations and prints JSON
observations, one per case. Use `--case <exact-id>` to select a case;
Python-only IDs additionally require `--include-python-only`.
`-B` prevents interpreter bytecode writes; the harness creates no files.
The optional unit checks protect all goldens, repeatability/input immutability,
source-drift rejection, deliberate comparison mutations, and offline escape
attempts. No subprocesses are used by either runner.
