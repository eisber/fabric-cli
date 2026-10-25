<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI JMESPath compatibility

Source: `microsoft/fabric-cli` at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Base: `eisber/fabric-cli` `feature/rust` exact
`e0be370e6aade9d61ea46561e36a53b0ddc9bdec`.
Implementation: [`fabric-cli-query`](../crates/fabric-cli-query/README.md),
CLI layer; no SDK or deployment behavior changes.

## Source mapping

| Source entry point | Translated or measured behavior | Native surface |
| --- | --- | --- |
| `utils/fab_jmespath.py::search` | Empty query inventory; `.` identity; real JMESPath; scalar `str`; structured errors | `search`, `search_with_options` |
| `utils/fab_jmespath.py::_get_json_paths` | Dictionary/array order, empty arrays/objects, indexed paths, default depth four and deep traversal | Iterative path traversal |
| `utils/fab_util.py::process_nargs` | List tokens joined with spaces, edge quotes stripped; single string not stripped | `join_query_args`; callers pass single strings directly |
| `utils/fab_cmd_ls_utils.py::format_and_print_output` | Real source query branch overrides default columns; empty query uses default columns | Source-executing caller test; command composition remains separate |
| `utils/fab_cmd_get_utils.py::query_and_export` | Real source inventory/dot/stringified-false branches; output truthiness | Source-executing caller test; file export not implemented |
| `commands/api/fab_api_request.py::exec_command` | Real source skips empty query and retains payload for empty projected list | Source-executing test with in-memory response only |
| `commands/find/fab_find.py`, `commands/acls/fab_acls_get.py` | Search callers inspected and blob-pinned | Inventory only, not full command parity |
| `errors/common.py::CommonErrors.invalid_jmespath_query` and `core/fab_constant.py` | UI message `Invalid jmespath query (https://jmespath.org)`, code `InvalidInput` | `QueryError` accessors; existing output failure selection |

`replace`/mutation consumers in `fab_cmd_set_utils.py` are deliberately excluded.
No source or old golden fixture is edited. The old workspace `query_projection`
expectation is consumed unchanged as a public-API regression.

The configured read host's generic GET now supplies additive SDK `ItemMetadata`
directly to this evaluator, preserving the original parsed HTTP key order and
number types rather than reserializing legacy `Item`. Both protected default
GET path inventories match source order. Shared-HTTP consumers exercise actual
identity, `keys(@)` and numeric-array queries; the query implementation and its
numeric/runtime limits are unchanged. Specialized GET URI differences remain
separate; see [read-command compatibility](read-command-compatibility.md).

## Bounded behavior matrix

| Area | Evidence | Label |
| --- | --- | --- |
| Query absent vs empty vs `.` | Original value vs ordered paths vs exact identity; scalar/list/object/falsy cases | Native exact |
| Field/quoted key/index/slice | Missing/null fields, negative and absent indexes, forward/reverse/step slices | Native exact within limits below |
| Ordinary language | Nested filters/projections, flatten, pipe, multiselect list/hash, wildcard, literals, logical operators | Native exact in frozen cases |
| Ordering | Input object keys, literal and multiselect keys, `keys`, `values`, object wildcard, merge order, stable `sort_by` | Native exact in frozen cases |
| Scalars | `None`, `True`/`False`, integers, `1.0`, `-0.0`, fixed/exponent floats, empty/raw Unicode strings | Native exact in JSON domain |
| All 26 standard functions | At least one real Python/native comparison for each; typed/arity errors and exprefs included | Native exact in tested domain |
| Python quirks | `contains([true], 1)` true; nested equality bool/number true; outer bool/number equality false; `to_number("001")`, whitespace/plus/underscores | Native exact |
| Existing output join | Every case through public `OutputEnvelope` and caller streams; complete bytes, status, exit and stream comparison | Native exact for supported cases; explicit failure for gaps |
| Errors | Malformed/whitespace query, unknown function, type/arity, bad sort keys and slice zero | Native exact source message/code and output |
| Wide integer numeric operations | Comparison/sort/min/max/by/contains/sum/avg rejected conservatively if their actual operands contain an integer with magnitude above 2^53 | Unsupported, not rounded success |
| Float summation dependent on Python runtime | Reject when simple and compensated results differ; Python changed `sum` in 3.12 | Unsupported |
| Generated numeric value outside JSON boundary | Non-finite `to_number`, unbounded integers, out-of-range ceil/floor/sum | Unsupported |
| Numeric literal outside i64/u64 | Scan original JSON literal tokens before library conversion, including nested literals | Unsupported |
| Deprecated unquoted backtick strings | Python's legacy behavior differs from strict-JSON library parser | Unsupported |
| Expression references returned as data | Python objects/reprs may be non-JSON or nondeterministic; only supported expref argument positions accepted | Unsupported |
| Extreme indexes/slices | i32 minimum/out-of-range index and step magnitude above 1,000,000 fail explicitly | Unsupported |
| Expression nesting | Library parser/evaluator limit 128, typed `ResourceLimit` | Explicit native safety limit |
| Arbitrary Python objects, NaN/Infinity JSON inputs, lone surrogates | Cannot be expressed by the public finite JSON `Value` input | Outside input domain |

The wide-integer guard is intentionally conservative within actual operands,
including unselected row fields passed to `sort_by`/`min_by`/`max_by`. It does
not reject a query merely because an unrelated input subtree has a wide integer.
Wide `to_number` results are conservatively rejected whenever the query contains
a numeric-sensitive operation. Direct identity, path inventory and
field projection retain exact i64/u64 values, including `9007199254740993`.
This limitation is not claimed as source parity or an approved Python behavior
change. Dependency upgrades or narrower exact numeric comparison support need
new evidence; never remove the guard merely to improve a denominator.

`to_number` Unicode decimal digits are also explicitly unsupported. Rounding,
Python-version-specific Unicode/numeric behavior and full upstream compliance
suites remain broader coverage gaps. A new parser/evaluator was not written.

## Independent evidence and denominator

The frozen input/expectation inventory has **118 cases**, labelled **108 native
exact / 118** and **10 unsupported / 118**. Unsupported Python results are
retained, not replaced by native expectations. Tests require a typed native
`Unsupported` diagnostic and the existing failure bytes for these cases.
All supported cases compare exact tree/scalar types and dictionary order, then
the complete output bytes and exit/stream observations.

The [oracle](../tests/compatibility/query/README.md) executes complete,
blob-verified Python definitions with installed `jmespath==1.0.1`, not a
hand-translated reference. It reuses the unchanged output oracle. Guard tests
exercise network/auth-import/process/sleep/write denial, source drift,
determinism, input immutability, missing cases, corrupted trees/bytes and
unsupported relabelling.

The first actual comparison exposed ten function differences rather than just
confirming a crate compiled. These were fixed with eight function adapters.
The dependency-selection PoC, subsequent comparisons and final targeted
commands run through the common parent OS mutex with private target directories,
single build/test threads and explicit release receipts. No native cross-platform
CI or integrated command parity is claimed by local query evidence.
