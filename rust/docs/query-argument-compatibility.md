<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Read query arguments

Source: `microsoft/fabric-cli`
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Base: `d3d42eca672b387cabc0626901bb3719bab76f74`.
This bounded follow-on preserves the public `ReadCommand` fields, types and
layout, including `query: Option<String>`. It changes no SDK/query evaluator,
manifest, dependency, production host, authentication, transport or namespace.

| Original source boundary | Native treatment |
| --- | --- |
| `register_ls_parser`, including `dir` | One scalar query argument; literal edge quotes retained |
| `register_get_parser` | One or more separate query/output tokens; existing `join_query_args` |
| `CustomArgumentParser` / argparse option actions | Last occurrence wins; known short clusters and unambiguous read-option abbreviations |
| argparse variadic values | Separate values stop at options; an attached value consumes only itself |
| argparse negative-number matcher | ASCII negative integers and decimals are values; exponent/trailing-dot/word forms require attachment or become syntax errors |
| argparse path positional | One contiguous variadic path; cannot resume it after an option |
| `--` | Ends options, never supplies a missing query value |
| `process_nargs` and LS query caller | Absent, scalar empty and present nonempty remain distinct parse meanings |
| Existing executable preview | Sanitized invalid-argument stderr/exit 2, help/version stdout, valid reads `NotConfigured` |

Clap still owns the bounded command/flag schema, defaults, cardinality, aliases,
help, value validation and error handling. A read-only lexical adapter derives
option arity/spellings from that schema to preserve the demonstrated argparse
group boundaries before Clap validates canonical arguments. It is not a wholesale
parser replacement or JMESPath implementation. Use public `parse_args` for these
source lexical rules; `command()` exposes the underlying schema/help.

## Source correction and query presence

The initial task's generic multi-token premise was not the source contract.
Native GET already accepted `get . -q properties | keys(@)` as separate argv
tokens. LS does not have that `nargs` policy. For example, raw tokens
`ls -q name | keys(@)` parse query `name` and path `["|", "keys(@)"]`;
`ls . -q name extra` is a syntax error. Joining those LS tokens would change
the meaning, not close a porting gap.

Scalar LS `--query ""` is false before `process_nargs` and uses default columns.
A synthetic source handler `query=[""]` is true before joining and invokes
empty-query path inventory. The original folder reference deliberately exercises
the latter handler boundary. GET preprocesses nargs first, so absent/empty both
select path inventory, although `None` and `Some("")` remain distinct parsed
values. Folder-context LS still ignores queries entirely.

The additive public `FolderReadHost::with_scalar_query()` selects scalar
truthiness for callers of `parse_args`; use
`host.with_folders(&executor).with_scalar_query()`. Its private wrapper policy
does not add fields to `ReadHost` or `ReadCommand`. The default wrapper keeps the
pre-existing handler-list empty-query behavior so the original public contracts
and 59 frozen handler cases stay intact. Raw probe consumers explicitly select
the scalar builder and pass argv without query joining. No case IDs, fixture
expectations, query rewrites or output callbacks select behavior.

## Evidence and limits

The [new independent oracle](../tests/compatibility/query_arguments/README.md)
executes actual source registration and processing, not a translated Python
parser. Its immutable inventory is 60 argument cases, independently source
verified, with 42 accepted and 18 rejected. Both native probes observe those same
60 cases. Seven new public-parser regressions and expanded binary sanitization
checks complement that comparison.

The 16 additional source consumer cases check exact requests/scopes,
native-query output bytes, streams and exits; seven GETs are observed in both
probes, giving 23 native observations, not 23 distinct source cases.
This does not expand the original 59 handler cases or 25 read cases, nor the
query engine's 108 exact / 118 and ten unsupported numeric/language cases.
Their inputs, expected outputs, labels and pins are protected and unchanged.

### Executed local evidence

Final serialized validation completed **October 25, 2026, 07:29:54 UTC** on
Windows `x86_64-pc-windows-msvc`, Rust **1.99.1**, Python **3.13.15**,
development `cachetools==5.5.2` and `jmespath==1.0.1`.

| Check | Observed result |
| --- | --- |
| New actual registration/native argument comparison | 60 source cases, 42 accepted / 18 rejected, exact in both probes |
| New actual helper/host/SDK/query/output comparison | 16 source cases / 23 native observations, all exact |
| New independent oracle controls | 16 passed |
| CLI native regression/architecture tests | 64 passed, including seven new parser and two scalar-host tests |
| Strict CLI all-target Clippy, workspace formatting, new oracle Black | Passed |
| Existing nine-stage actual CI comparison runner | All stages passed, unchanged |
| Protected read/metadata/source-route helpers | 25 outcomes: 22 exact / 3 safety; four raw cases; 176 source helper observations / 124 native outcomes; 60 guards |
| Protected native query | 108 exact / 118; ten unsupported, 15 guards |
| Protected folder host | 59 native: 32 exact / 21 safety / 3 snapshot / 3 unsupported; 54 terminating source / five nonprogress proofs; 36 guards |
| Existing CI runner controls | Six passed |

All heavy work used the common `Local\FabricRustParityValidation` mutex,
private target/development environment, one build job and one test thread.
Total acquired gate time across all attempts was **770.659 seconds**; no command
exceeded ten minutes. `final-receipt-4.json`, `validate-final.ps1` and complete
`final-validation-output.log` are retained in the assigned session artifacts.
Earlier failed receipts remain: missing development dependencies, guarded
argparse formatting seams, an initial Clippy iterator warning, a PowerShell
logging parameter, and a new probe's incorrectly nested item-context setup.
The actual source/native comparisons found the latter; no fixture was changed
to hide it. Source-only consumer expectations were frozen before native runs.

These are local targeted results, not a new full-workspace or cross-platform
CI result. The parent owns full integration and wiring the new reference into
native CI after reviewing the exact local commit.

The preview has no live executable host. Definitions/export, text/REPL,
unported Python options, full argparse/Unicode decimal classification and
Python global-output-default semantics remain outside this measured slice.
Source quirks and proposed consistency changes are discussed separately at
`tk:post/12/4/97`, following `tk:post/12/4/96` and `tk:post/12/4/94`;
discussion is not approval to change source behavior.
