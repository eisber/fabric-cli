<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared text and definition output branch ledger

Python source pin: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Additive implementation in `fabric-cli-output`, adopted integration checkpoint
`e850b32437e0e9171e8f097bfbddb93d57986c3f`. Main/parser and command families
remain separately owned. Existing `OutputInput`, `OutputEnvelope`, `RenderPlan`
and numeric exceptions remain compatible.

Raw-definition API committed as `f2763ab33de8b78ee467a64004128c1234b69175`;
single-core extraction and reviewed architecture exception committed separately
as `9a8e6b091b09368bd31ab143767b8a7dca6892ec`. Core and CLI each passed the
12 raw source byte cases; CLI retained all 42 original envelope byte/tree/stream/
exit cases. Core consumers2, CLI consumers7, architecture10 and strict targeted
Clippy passed. Core receipt released at 2026-10-26T01:17:54.578Z.

Text evidence is separate: **83 actual-source byte/stream/exit/error comparisons**,
five text consumer tests, seven retained JSON/definition consumer tests, one
doctest, eight Python guard/source tests and strict all-target output Clippy.
The comparison also retains source partial output before a later render error.
This is a caller-injected pure rendering foundation, not full interactive
terminal/command parity.

| Source branch | Public boundary / treatment | New fixture coverage |
| --- | --- | --- |
| `fab_util.dumps` default `indent=None` | `json_dumps(&Value)`, raw value, comma-space and colon-space, no envelope/wrapping/newline; follow-on shared core extraction keeps compatibility wrapper | 12 manually pinned definition byte cases; real `json.dumps` plus source-pinned `dumps` comparison passed |
| `print_output_format` JSON/text/config/default/unsupported selection | `OutputFormat::resolve` + `text::render`; explicit caller config, text flags ignored in JSON | selection, empty explicit fallback, unsupported format, JSON ignores truncation |
| `OutputResult` falsey/empty text | Reuses normalized existing envelope; `InvalidResult` only when all three result inputs absent | false, zero, empty dict/list/message/hidden data |
| `_print_raw_data`, `_print_dict`, `_print_simple_items` | Single dict unwrapped, multiple dicts pretty JSON2, otherwise Python JSON-domain `str`/`repr`; caller formatter for Unicode nested repr | dict/list/scalar, bool/None/nested values, quote/control escapes, injected Unicode printability |
| `_print_output_format_result_text` routing | command/subcommand ls/dir or forced headers precede key/value; no keys fall back to raw | alias/subcommand/forced header/precedence/no-fields cases |
| `print_entries_unix_style` widths | First-row fields/order, visual widths for headers versus codepoint lengths without headers, +2 padding, shrink/clamp/wrap/fallback | empty table/fields, width0/1/narrow, heterogeneous errors, Unicode wide/combining |
| `truncate_columns` | Private clone, cap50, shrink in priority order, min20, source codepoint ellipsis/right trim; separate width120 fallback | absolute cap, overflow, missing columns, priority, truncation-before-raw, JSON untouched |
| `_format_key_to_convert_to_title_case` | ASCII title/camel/space/underscore validation and Id/CLI/PowerBI substitutions; Unicode caller formatter | valid/spaces/numbers, validation branches, row error, injected Unicode key |
| hidden and done | Separator on stderr, sorted hidden values on stdout; source success marker/newline and HTML-fallback quirk | hidden strings, message punctuation/markup, fallback, mixed-stream order and writer failure |
| text errors/warning/info/progress/done | Dedicated render helpers, exact default streams/exit, punctuation/command rules; text-only Fabric/OneLake/Azure decorations from selected fields | ordinary/Unauthorized/fallback; detail verbose/default/missing ID, Azure/OneLake optional ID/time; stdout success and stderr notices, string-zero/empty progress |
| `_safe_print` / formatted print | Pure byte plans with caller TTY, style palette, fallback and newline; no ambient terminal | injected fake style bytes, no-TTY suppression, explicit CRLF; writer failures |
| `display_help`, `print_version` | Explicit caller-ordered categories/header/version; no namespace/registry ownership | default/custom/empty categories, empty-command error and partial intro, exact version lines |
| raw plain/grey/Fabric helpers | Explicit style/stream `render_line` boundary | grey stderr, Fabric color, plain fallback |

## Failure-envelope presentation

`text::render` routes a Failure envelope through the existing `render_error`
path before applying success data, table, hidden-item or message formatting.
Read, job, table, local and resource consumers therefore retain the `x` marker,
full error command path and `[code]` on stdout. Exact `Unauthorized` exits 4;
other command failures exit 1. Already-normalized message/code fields are not
passed through `CommandError::new` again: empty normalized messages, final
periods and operation/OneLake details remain unchanged.

JSON rendering still uses the original envelope serializer before text routing.
Successful text options and source-silent results remain unchanged. Table
diagnostics retain their earlier stderr records, and caller-owned writers still
propagate the first typed I/O error without fallback, flush or operation replay.
The separate auth/bootstrap stderr schemas and correct access-family error
renderer are not rewritten.

The additive frozen Python captures and `output_audit`/`output_contract_audit`
tests cover the presentation regression and its consumers. Their presence is
not a claim of native execution or full command parity. Format defaults and
Unicode providers remain separate compatibility boundaries.

### SRC2: local success command names

The local adapter passes Python's `args.command` to successful output:
`config get`, `config set`, `config ls` and `config clear-cache` use `config`.
The typed local-command mapping is separate from `command_path`; failures keep
the complete leaf path in text and JSON. No envelope serializer, filesystem
handler or configuration-format selection changes. The frozen `config-value`
capture remains unchanged. Mounted executable tests cover explicit text/JSON,
all four config leaves, absent-value silence, real setting/context files and
failure paths. This repairs grouped local output, not all grouped command
families or complete Python terminal parity.

The mounted PWD fixture separately follows source `_print_context`'s OS cwd
contract: physical directory spelling on POSIX and native cwd spelling on
Windows. Its exact parsed-JSON assertion and portable parent-component cwd
regression do not change production path resolution, containment or symlink
behavior. Pinned `commands/fs/fab_fs.py::_print_context` uses `os.getcwd()`;
the native local host injects `std::env::current_dir()`.

### SRC3: silent empty GET projections

The generic item GET consumer now preserves `query_and_export`'s conditional
printing **after** query evaluation. Empty lists, objects and strings return
no output input, not a successful empty envelope. Source query results for
false, null/missing and zero are the nonempty strings `"False"`, `"None"` and
`"0"` and therefore remain visible. The shared envelope/query libraries and
LS behavior are unchanged: JSON LS still emits `Success` with `data: []`.
The existing job empty-result message is not changed or suppressed. In the
adopted source and current job host it is a successful `"No runs found"` message,
not a `NoRunsFound` failure code.

The folder wrapper already delegates GET's optional output unchanged; no
additional envelope or folder request is introduced. Additive real-query
consumer tests cover empty/container/scalar distinctions, both text/JSON
production-session outputs, folder delegation, exact request counts/scopes,
query/service failures and caller-owned writer failures. The original frozen
`get-query-empty-source` capture and mounted failing regression are retained.
These are bounded output repairs, not completion of GET export, definitions,
enrichment, metadata operations or live host acceptance. Focused validation
uses `local_mounted`, `folder_reads`, and the native-read
`output_contract_audit`, `read_session` and `job_read_session` test targets.
Native after-fix CI remains required.

## Explicit boundaries still requiring completion

This slice does not certify full text parity. Unicode width is injected as the
source East Asian F/W property (2, otherwise1), **not wcwidth/grapheme width**.
The included ASCII provider explicitly rejects non-ASCII table width; tests
inject the actual widths for their synthetic Unicode characters. A production
caller must supply complete pinned Unicode data. `TextOptions::key_formatter`
and `value_formatter` similarly accept source-equivalent key casing and Python
str/repr behavior. Default helpers explicitly reject non-ASCII keys/nested repr
strings rather than guessing; top-level raw Unicode strings remain unchanged.
Source comparisons demonstrate injected Unicode title and printable versus
nonprintable repr cases, not a built-in all-Unicode database. Complete provider
composition remains a caller integration requirement.

Color escape sequences are caller-supplied semantic-style framing; fake palette
fixtures prove routing/suppression, not byte equivalence to every real
prompt-toolkit backend. The library neither emulates a terminal framework nor
detects TTY, encoding, width or environment. Newline injection does not normalize
existing fixtures. Source print fallback is a caller-selected policy, not an
I/O retry: actual `Write` failures propagate.

Structured Fabric/OneLake/Azure error response parsing remains outside this
library. `render_decorated_error` consumes already-selected code/message/request
ID/time/detail fields and implements the source display branches; it does not
extract IDs with SDK/regex logic. Raw HTML-like tags/entities in unescaped source
error codes/request IDs are not certified by these plain-field fixtures.
Arbitrary terminal toolkit HTML and backend failure/AttributeError rethrow are
not emulated. Actual writer errors propagate, never trigger fallback writes.
The empty-help `max()` error wording is pinned to the tested Python interpreter;
other Python versions may use different builtin diagnostic text.

The parent requested a **single core-layer encoder** for SDK raw
request bodies as well as definitions. The separate validated commit extracts existing
encoding into `fabric-core-json`; CLI API types remain wrappers, not copied
serializers. Registry versions/features/checksums stay unchanged. This introduces
the exact `fabric-cli-output -> fabric-core-json` edge, approved narrowly and
tested by the architecture contract. Generic layer permissions remain unchanged,
including denial of other CLI-to-core edges and forbidden core transitive edges.

## Source-discovered quirks and oracle corrections

The new text oracle caught a real implementation defect: hidden-item separator
uses `print_grey`'s default **stderr**, whereas `_print_raw_data(hidden_data)`
uses stdout. `TextPlan` therefore preserves ordered per-stream records rather
than changing the old single-record JSON plan. The newly authored text case's
initial handwritten stream assumption was corrected to actual source evidence;
no previously committed golden or old source expectation changed.

Source may print valid key/value entries before a later invalid key/row raises.
`TextError::AfterOutput` retains that partial plan instead of erasing side
effects; `write_to` stops at the first writer error. Headless fake collaborators
prove routing and byte composition, not a real terminal's encoding/style policy.

`text_reference.py` executes real source definitions in independent namespaces,
with pinned UI/utility/error source hashes. Width/config/print/toolkit
collaborators are private, no global module/environment mutation. The oracle
reads new cases and actual Rust observations; it never generates Rust expected
output or updates existing goldens.
