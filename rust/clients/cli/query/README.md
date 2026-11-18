<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Query safety boundary

`validate()` is offline preflight: syntax, supported AST forms, function names,
and static arity are checked before the host acquires credentials or sends a
mutation. It reuses the actual evaluation runtime, not a second signature table.
The pinned jpx `Function` trait does not expose signatures, so preflight probes
each function with null placeholders: runtime signatures check arity before
types, and functions accepting null perform only trivial work. Type errors
remain data-dependent. Both branches and expression references are checked,
even when a branch might not execute. Structural AST depth uses the same limit
as admission, so parser-accepted but over-depth queries also fail preflight.

Backtick JSON literals use the shared syntax-preserving JSON decoder. The pinned
jpx lexer deserializes directly into serde `Value`, whose arbitrary-precision
visitor can mistake a genuine `$serde_json::private::Number` object member for
an internal numeric marker. Query parsing masks decoded literal interiors with
byte-length-preserving placeholders, then restores their AST values before
validation/evaluation. This preserves legitimate root, nested, and escaped keys;
it does not blacklist object members. Raw strings and quoted identifiers remain
on the native string-only parser path.

## Deliberate resource limit

jpx-core **0.4.0** exposes a recursion limit, but its `Expression::search`,
`Context`, and `Function` APIs provide no allocation/work budget hook. Identity,
multiselects, function arguments, projections, and expression references can
clone data. A response-size limit or final rendered-size check does not bound
these intermediate values.

Before entering the native interpreter, `budget.rs` performs conservative AST
admission without constructing result JSON:

- Query source: at most **64 KiB**.
- AST and expanded JSON nesting: at most **128** levels.
- Cumulative expanded copy weight: **64 MiB**, including discarded pipe stages,
  temporary storage allowances, and repeated expression-reference invocations.
  Weight is 64 bytes per JSON node plus up to six bytes per source string/key
  byte for escaping. AST storage is also reserved. This bounds expanded nodes
  and payload, not allocator-specific resident memory.
- Analysis/evaluation work allowance: **1,000,000** conservative traversal and
  comparison units, with repetition charged for projections and functions.

Each node's expanded size is bounded using saturating arithmetic. Known arrays
are inspected per element, retaining actual selected-property sizes: one row's
large unused field is not multiplied across all rows when projecting a small
field. Constructed/unknown arrays retain conservative width/largest-child
bounds; every row may survive a filter. Functions include their growth
(notably `join`, `to_string`, and `map`) and
native temporary copies. Builtins charge their actual storage shape: scalar
`length` does not reserve another full array copy, and sorting comparisons
consume work rather than fictitious JSON-copy bytes. Expression-reference bodies
are analyzed with their full invocation multiplicity. New unmodelled native
functions fail closed.
The admitted query still executes in jpx with the existing Python adapters;
the admission model does not replace JMESPath evaluation semantics.

Admission failure is a typed `QueryErrorKind::ResourceLimit` with the usual
redacted `InvalidInput` message. It occurs **before intermediate materialization**,
not merely after rendering. For example, repeated `[@,@]` pipes are denied even
when followed by `length(@)` or a constant result. Tests exercise small budgets;
do not run a large allocation reproducer.

Bounds deliberately overestimate unknown values, both sides of conditionals,
filter survival, and maximum row sizes. Thus some valid, inexpensive queries
can be rejected. This is a safety difference from Python, not an unsupported
syntax blacklist. Admission depends on response data and cannot generally be
completed before a remote mutation. Syntax/name/arity/structural-depth preflight
can and must.
Absent-query passthrough, `.` identity, and the empty-query path-inventory wrapper
do not enter the jpx interpreter and retain their existing behavior.

## Integer precision

Exact arbitrary-width JSON integers remain exact in field/index/container
projections, scalar formatting, and `to_string`. Existing input guards remain
in place. `min_by`, `max_by`, and `sort_by` compare evaluated signed or unsigned
64-bit integer keys exactly, including alongside finite floats. Integer keys
outside that range fail with `Unsupported` before native comparison. Finite
float formatting still uses Python's binary64 normalization. Oversized integer
*query literals* retain the existing explicit unsupported boundary.

## Offline verification

From `rust/`, after the integration owner grants a heavy-build lease:

```text
cargo test --locked -p fabric-cli-query
cargo clippy --locked -p fabric-cli-query --all-targets -- -D warnings
```

The host should additionally test POST preflight with `not_a_function(@)`,
`length()`, and invalid calls nested in expression references. Assert
`InvalidInput`, zero credential-provider calls, and zero HTTP requests. Check
the same zero-effects mapping for `vec!["a"; 129].join(".")`. Check
successful field/index/container projections and output encoding of exact
`18446744073709551617`, `-9223372036854775809`, and integers beyond i128.
