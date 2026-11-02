<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Command-line parser design and choices

## Recommendation and scope

Retain **Clap's builder API**, with typed command-family inputs and a thin,
source-backed compatibility adapter where Python's argument rules differ.
Compose family registrations in one front door. Do not replace the library,
introduce a registry-generation framework, or have multiple contributors edit
the central parser to accelerate command delivery.

This is a design explanation and integration recommendation, not a parser
implementation or approval of a new public API. Sections labeled **implemented**
describe the pinned source below. The proposed integration contract and backlog
are not claims that those changes have landed.

The current owner priority is working, happy-path command parity first.
Stricter handling of malformed input can be an explicitly documented preview
difference while diagnostic and obscure lexical compatibility are refined.
That prioritization does not authorize changing frozen expectations, removing
known failures, accepting a valid command with the wrong meaning, or claiming
whole-CLI parity.

## Evidence baseline

| Evidence | Exact baseline and interpretation |
| --- | --- |
| Rust implementation | [`eisber/fabric-cli`, `9c396cef44b736733a635070431d871c8f18762c`][rust-pin], published `feature/rust` checkpoint |
| Python behavioral reference | [`microsoft/fabric-cli`, `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`][python-pin], not whichever upstream commit a contributor happens to have checked out |
| Dependency and toolchain declarations | [Workspace manifest][rust-manifest] declares Rust `1.99.1` and Clap version requirement `4.6.7`; the [lockfile][rust-lock] resolves `clap` and `clap_builder` to `4.6.7` |
| Published CI | [Run `40380004702`][ci]: provider reports completed/success at the exact Rust SHA, with successful native checks on `windows-2025`, `ubuntu-24.04`, and `macos-15`, plus the final Rust validation job |
| This document's inspection | October 26, 2026; read-only Git-object/source inspection, workstream reference reads, official library documentation, registry release metadata, and existing CI status. No new build, parser execution, test run, dependency installation, or live service operation |

CI success is evidence for the checks that run at that revision. It is not proof
that every Python command, platform credential backend, malformed input, REPL
interaction, or live service scenario is compatible.

This document was authored on a lane inheriting upstream
`8ad4c703bdd6738189f59318a04cab297b6c4381`, without the Rust workspace checked
out. Rust and oracle files were read from their exact existing Git objects;
the checkout and Python source were not reset or rewritten. Relative links to
Rust documentation below refer to the integration workspace.

Prior art is the [architecture](architecture.md),
[migration guidance](migration-guidance.md),
[compatibility backlog](compatibility.md),
[command inventory](command-parity.md), and
[query argument compatibility record](query-argument-compatibility.md).
Some existing documents contain historical checkpoint assessments. The exact
source pin above, rather than an older status paragraph, governs the implemented
behavior described here.

## What is implemented

### The parser is one layer, not the command runtime

The current flow is:

```text
Process arguments, including argv[0]
    -> parser::parse_args
       -> Clap schema plus bounded read lexical adaptation
       -> Result<Invocation, clap::Error>
    -> executable dispatch
       -> display text, configured read host, or opt-in auth host
    -> service execution and presentation
```

[`parser.rs`][rust-parser] exposes two distinct entry points:

- `command() -> clap::Command` constructs the schema and help metadata.
- `parse_args<I, T>(args: I) -> Result<Invocation, clap::Error>`, with
  `I: IntoIterator<Item = T>` and `T: Into<OsString> + Clone`, also applies the
  measured Python read-token rules. Arguments include the executable name.

Calling `command().try_get_matches_from(...)` directly is not equivalent to
calling the public compatibility parser. It bypasses the read adapter.

The implemented `Invocation` variants are `Display(String)`,
`Read(ReadCommand)`, and feature-gated `Auth(AuthCommand)`.
`ReadCommand` carries operation kind, original alias spelling, path tokens,
query presence/content, flags, export-path selection, and output override.
[`AuthCommand`][rust-auth-parser] carries non-secret account/tenant/client selectors, action,
timeout, and permission for interaction. Neither DTO acquires a credential or
executes a service command.

[`main.rs`][rust-main] consumes the typed result:

- Help/version goes to stdout without configuring a service host.
- Default and broker-only builds recognize reads but return `NotConfigured`.
- `native-read` explicitly supplies the bounded Windows read host.
- `windows-broker` adds auth dispatch; parsing itself never signs in.
- Invalid syntax becomes a static JSON diagnostic on stderr with exit 2.
  Raw Clap error text is not printed, because it can include supplied values.

Parsing requires no service I/O, account discovery, mutable global context,
credential environment variables, configuration-file loading, or async runtime.
The caller supplies argv explicitly. The compile-time package version is
metadata, not runtime credential/configuration discovery.

Service hosts are separate. The existing `ReadHost` takes injected SDK,
executors, lookup/context, projection, configuration, time, and writers.
[`ReadSession`][rust-session] adds async snapshot resolution and text/JSON rendering.
The executable's native host configures its environment-dependent dependencies
after parsing. Auth selector validation and broker execution likewise belong
to the auth host, not to the syntax parser.

### What Clap owns, and what our adapter owns

The workspace selects Clap with `default-features = false` and
`std`, `help`, `usage`, and `error-context`. It does not enable `derive`, `env`,
`string`, color, or suggestions. The lockfile contains `clap_builder` and
`clap_lex`, but not `clap_derive` or `clap_complete`.
These are the selected features, not a recommendation to disable Unicode
argument values or a claim that every linked dependency has zero cost.

| Responsibility | Implemented owner |
| --- | --- |
| Command names and nesting | Clap `Command` construction |
| Aliases and generated schema/help | Clap; currently `ls` has visible alias `dir` |
| Option names, short flags, value counts, choices, typed numeric ranges | Clap `Arg`, actions, `num_args`, and value parsers |
| Repeated option policy | Explicit `args_override_self(true)` on bounded read commands; not an assumed library default |
| Unambiguous long-option prefixes | Explicit `infer_long_args(true)` on bounded reads, with matching adapter handling |
| Read-token grouping and Python-specific option/value boundaries | Private `source_read_arguments` adapter |
| GET token joining and edge-quote stripping | Existing query/context processing helpers, reflecting Python `process_nargs` |
| Help/version versus command result | Public `Invocation` conversion |
| Executable stream, exit, and secret-safe syntax error policy | `main.rs`, not Clap's default process-exiting convenience path |
| Identity, HTTP, context lookup, job polling, file reads, cancellation, rendering | Explicit hosts and existing focused libraries, not parsing |

The adapter consults the built Clap schema for option spellings, actions, and
arity. It normalizes the bounded read tokens before final Clap validation.
It does not maintain an unrelated second list of read options, evaluate
JMESPath, resolve paths, or implement service behavior.

That division is useful but not free: we own the adapter's correctness and
source fixtures. Adding a new option can introduce abbreviation ambiguity or
change token boundaries even when the Clap builder change looks small.
Unknown or ambiguous tokens are not silently accepted; the normal parser error
path remains observable.

### Why builder rather than derive today

The source uses `Command::new`, `.arg`, `.subcommand`, conditional registration,
and a loop for auth actions. This fits independently supplied command families
and feature-dependent composition. It also exposes the schema that the read
adapter inspects. The currently selected dependency features omit proc-macro
derive support.

This is a rationale for retaining the implemented design, not evidence of an
earlier exhaustive library competition. There is no measured result here saying
builder parses faster or always compiles faster than derive.

Clap derive is another way to construct the same underlying schema. It is useful
for static option structs and subcommand enums, reduces manual field extraction,
and supports `Args`, `Subcommand`, and `FromArgMatches` composition with builder
code. It does not automatically reproduce Python tokenization or defaults.
Type-derived cardinality, naming, requiredness, and defaults still need checking
against the source. A `Vec<String>` is not evidence of Python `nargs` parity.

Builder composition here means assembling a command tree in ordinary Rust.
It does not mean a runtime plugin loader or loading arbitrary JSON command
definitions. Current names are static strings; arbitrary runtime-owned schema
strings can require Clap's separate `string` feature.

## Why Python compatibility needs more than a plausible schema

The reference is Python's actual
[`create_parser_and_subparsers` and `CustomArgumentParser`][python-setup],
the [read registrations][python-reads] and [global flags][python-globals] it
installs, and subsequent command processing.
The [AST extractor][inventory-extractor] inspects those registrations without
importing product code or discovering credentials.

The committed [inventory][inventory] contains **52 canonical leaves, six
groups, and 11 alias paths**: 26 root leaves plus 26 grouped leaves.
Aliases are additional spellings, not additional command implementations.
The groups are `auth`, `config`, `acl`, `label`, `job`, and `table`.
This denominator excludes the many option, resource, mode, output, and failure
branches within each command.

### Concrete read-token examples

The following are **argv arrays after shell processing**, not shell command
strings. Literal quotes in an array element are characters delivered to the
program. An unquoted `|` in a real shell would normally be a pipeline operator.

| Tokens after `fab-rs` | Implemented meaning or outcome |
| --- | --- |
| `["ls", "-q", "name", "\|", "keys(@)"]` | Query is scalar `"name"`; path is `["\|", "keys(@)"]` |
| `["get", ".", "-q", "properties", "\|", "keys(@)"]` | GET query tokens join to `"properties \| keys(@)"` |
| `["ls", "-q", "'name'"]` | Query retains literal edge quotes |
| `["get", ".", "-q", "'name'"]` | GET's list processing strips edge quotes, producing `"name"` |
| `["get", "."]` versus `["get", ".", "-q", ""]` | `None` versus `Some("")`; presence is retained |
| `["ls", "-q", "old", "--query", "new"]` | Last occurrence wins, rather than concatenating expressions |
| `["get", ".", "--query=name", "tail"]` | Invalid: attached value consumes only itself and the earlier path cannot resume |
| `["get", "-qname", "."]` | Attached query `"name"` followed by the first path group is valid |
| `["get", ".", "-q", "name", "-1", "-.5"]` | Separate variadic query includes ASCII negative-number values |
| `["ls", "--query=-1e3"]` | Attached value is valid; separate `-1e3` is classified differently |
| `["ls", ".", "-l", "tail"]` | Invalid: a variadic path already consumed cannot resume after an option |
| `["ls", ".", "--", "-q"]` | `-q` is a path token, not an option |

The distinction is not just quote removal. Python LS registers a scalar query;
GET registers `nargs="+"` for query and export output. Separate variadic values
consume until an option, but attached values consume only the attached token.
Python's variadic positional group is not freely resumable after an option.
The adapter and existing [`process_nargs` translation][python-util] preserve
these measured distinctions.

The ASCII negative-number classifier covers negative integers and decimals,
including `-.5`. Exponent, trailing-dot, and word forms do not receive the same
separate-value treatment. Full argparse classification, including Unicode
decimal-number cases, is not established.

Empty query semantics also belong to the caller boundary. Scalar LS's empty
query is false before `process_nargs`; an older synthetic handler input
`query=[""]` is a nonempty list before joining. Raw folder consumers select
`with_scalar_query()` explicitly; the older handler-list oracle remains intact.
GET preprocesses its list first. Normalizing all these cases into the same
"no query" value would erase behavior.

### Quoting, batch, and REPL are separate layers

For noninteractive use, the shell/process-launch boundary and platform runtime
already deliver argument tokens through `args_os()`. On Windows, command-line
decoding also involves the target runtime; it is not identical to POSIX shell
quoting. Do not join argv into a string and shell-split it again.

A shell-splitting library solves a different problem from Clap: turning a REPL
line or batch command string into tokens. It does not replace subcommand
selection, value counts, choices, defaults, help, or typed conversion.

The pinned Python [`main.py`][python-main] has a repeatable root
`-c`/`--command` registration and batch dispatch, whose command strings use
`command.strip().split()`. Its [`InteractiveCLI`][python-interactive] uses
`shlex.split(command.strip())`. These are observably different quoting rules.
Batch also has separators, summary output, and exit propagation.
A future Rust REPL/batch implementation should take source-derived fixtures for
each mode, not assume one splitter "fixes" both. Neither mode exists in the
pinned Rust front door.

### Remaining root and presentation surfaces

| Surface | Python reference | Rust at the pinned checkpoint |
| --- | --- | --- |
| Version | Root `-v`/`--version`, plus `version` leaf | `-V`/`--version`; no `version` leaf. GET's `-v` means verbose |
| No arguments | Enters the interactive REPL | Invalid-invocation stderr JSON, exit 2; no REPL |
| Group without leaf | All six source groups have help handlers | Only opt-in auth is in this root; bare auth requires a subcommand. Jobs has a separate library registration |
| Help | Root/group custom help, aliases, examples, learn-more sections, `-h`, `--help`, and `-help` | Generated preview help and display handling; not byte-identical custom help |
| Batch | Repeatable `-c`/`--command` plus main-loop behavior | Not registered |
| Output override | `--output_format {json,text}` added to each custom parser | One global Clap argument; schema accepts both, host determines support |
| Global defaults | A source subparser default can erase a root-only output override | Explicit Clap global value propagates; this difference is documented in the raw-argument oracle |
| Hidden skill | Hidden `--skill`, absent attribute via `argparse.SUPPRESS` | Not in root reads/auth; jobs library carries `Option<String>` and hides it from help |
| Aliases | Eleven paths across filesystem/config/ACL/label commands | Root read parser implements `dir`; the full alias inventory is not integrated |
| Abbreviations | Argparse's source-specific option inference | Bounded read prefixes, not a whole-registry guarantee |
| Completion | Python calls `argcomplete.autocomplete`; completion behavior includes source-specific completers | No integrated Rust shell-completion dependency or command at this checkpoint |
| Text output | Python formatting, warnings, tables, prompts, streams | Shared text rendering and `ReadSession` exist; original `ReadHost` is JSON-oriented. This is not full executable text/REPL parity |
| GET export/verbose/force | Source definition, export, and confirmation behavior | Recognized typed options; current bounded read hosts explicitly reject these unported behaviors |
| Authentication | Several source methods and source-specific defaults | Feature-gated explicit Windows broker selectors/actions, not a full Python auth port |

Auth registration provides a useful guard against accidental cross-family
parsing: successful auth matches are converted before the read lexical adapter.
Tests use accounts literally named `ls`, `dir`, or `get` and read values named
`auth`. The auth converter also rejects the read-only output override instead
of ignoring it. Future families need the same value-versus-command separation.

Completion must remain distinct from parsing. Schema-derived static completion
could cover names and choices; workspace/item completion would need an explicit,
bounded context/provider policy. Completing a line must not silently sign in,
print secrets, or turn help into a service operation.

## Alternatives and tradeoffs

The following versions were checked against crates.io release metadata on
October 26, 2026. They are comparison references, not proposed dependency
changes. Release age is not a quality or maintenance score.

| Library | Checked stable version | Registry release date (UTC) | Official reference |
| --- | --- | --- | --- |
| Clap | 4.6.7 | 2026-09-14 | [API][clap-docs], [features][clap-features], [derive][clap-derive], [registry][clap-release] |
| bpaf | 0.9.28 | 2026-09-17 | [API and features][bpaf-docs], [design][bpaf-design], [registry][bpaf-release] |
| argh | 0.1.19 | 2026-03-16 | [API][argh-docs], [typed parsing][argh-from-args], [registry][argh-release] |
| lexopt | 0.3.2 | 2026-02-28 | [API][lexopt-docs], [project documentation][lexopt-project], [registry][lexopt-release] |
| pico-args | 0.5.0 | 2022-06-04 | [API][pico-docs], [project limitations][pico-project], [registry][pico-release] |

### Capability fit

| Choice | Nested commands, shared options, and types | Help, completion, and introspection | Python compatibility implications |
| --- | --- | --- | --- |
| **Clap builder** | Explicit nested `Command` trees, global arguments, typed value parsers; manual conversion into family DTOs | Generated help/usage, inspectable command/argument schema; optional separate [`clap_complete`][clap-complete] | Best fit for existing composition. Still requires measured source adaptation; global propagation is not Python's repeated-parser defaults |
| **Clap derive** | Typed structs/enums, reusable `Args`/`Subcommand`, builder interoperability; same parser underneath | Same schema/help/completion ecosystem | Reduces field extraction for static shapes, but inferred defaults/cardinality must be overridden where needed. Not a lexical-compatibility switch |
| **bpaf** | Combinators and optional derive produce typed values; nested commands and reusable parsers; shared-option scoping is explicit composition | Generated help; optional `autocomplete` and `docgen`; parser composition/document generation, not a drop-in Clap `Command` graph | Credible typed alternative with flexible escape hatches. Ordering/scoping invariants and source behavior would need fresh evidence |
| **argh** | Derive-first structs/enums; nested and dynamic subcommands; `FromArgs::from_args` returns a result | Generated opinionated help; [`ArgsInfo`][argh-info] exposes structured argument metadata; no equivalent integrated shell-completion solution established by this comparison | Smaller-policy-surface candidate, but its Fuchsia-oriented conventions are not argparse compatibility. Verify inherited/global option placement rather than assuming Clap semantics |
| **lexopt** | Imperative stream of options and `OsString` values; caller builds DTOs, nesting, global scope, and cardinality | No generated command schema/help/completion; application owns those surfaces | Fine control at the lexical layer, but replacing Clap would transfer most validation/help/registry work into our code |
| **pico-args** | Imperative extraction of flags/options/free arguments/subcommands; caller constructs DTOs and nested/global policy | No help generation or declarative command graph; completion/introspection would be application work | Its documented extraction-order dependence is a poor default for precise option boundaries; feature flags affect attached values and short clusters |

For bpaf specifically, named parsers must precede command/positional parsers in
composition unless using the documented alternative restrictions. Its
[`OptionParser` documentation][bpaf-options] recommends invariant checks and
shows different behavior for same-named outer and inner options. This is not
a reason to reject it universally, but it rules out treating migration as just
renaming builder methods.

Argh should not be dismissed as "no introspection" or "no dynamic commands":
its official API includes both structured metadata and dynamic subcommands.
Conversely, their availability does not demonstrate compatibility with this
CLI's six groups, shared defaults, aliases, or lexical fixtures.

### Dependency and compilation considerations

Clap's currently enabled builder features avoid adding its derive proc-macro
dependency and optional completion component. Derive would add compile-time
macro work, but might reduce application boilerplate. bpaf also offers a
non-derive path and optional derive/completion/doc features. Argh is derive-first
and advertises code-size-oriented design. Lexopt advertises one source file,
no dependencies, and no macros. Pico-args deliberately has a small feature set,
with features controlling `=`, attached short values, and combined flags.

These are qualitative design/graph differences, not measurements of this
workspace. There are no cold/warm build timings, startup measurements, binary
size comparisons, memory figures, download rankings, or universal "fastest"
claims in this recommendation. A smaller parser dependency can produce more
application-owned help, dispatch, validation, and test code.

If a replacement is considered, use the same command inventory and frozen
source cases, the same compiler/target/features, and comparable full binaries.
Measure build/startup/size only against an agreed need. Prefer a bounded
experiment with an explicit incompatibility or resource target over a library
switch motivated by a generic benchmark.

## Proposed parse-to-dispatch contract

**Proposal, not implemented:** stabilize the existing shape, not a new registry
framework. The central integration owner is the only writer of root
composition, the top-level `Invocation`, and executable dispatch.
Family contributors own family registration, typed conversion, handlers, and
their source-backed cases in focused crates/modules.

### Keep this boundary

| Boundary | Concrete contract |
| --- | --- |
| Public parser input | Keep the existing generic `parse_args` signature, including argv[0] and `OsString` input |
| Public parser result | Keep `Result<Invocation, clap::Error>`; successful display is `Invocation::Display`, not a process exit |
| Read/auth inputs | Preserve existing `ReadCommand` and feature-gated `AuthCommand` contracts; do not add runtime objects to them |
| New family input | One owned family DTO, such as existing `fabric_cli_jobs::JobCommand`, carried by an explicitly added `Invocation` variant |
| Family schema | Reuse `job_subcommand() -> clap::Command` for jobs; other groups expose an equivalent subtree constructor, while root leaves may use a root-registration adapter |
| Family conversion | Reuse `command_from_matches(&clap::ArgMatches) -> Result<JobCommand, CommandError>` for jobs; pass group matches, not the entire root |
| Service boundary | Handlers consume family DTOs and injected host capabilities; SDK/service libraries do not consume Clap `ArgMatches` |
| Syntax/conversion failure | Central integration maps family conversion failures to classified parser errors without stringifying values; host/service errors remain separate |
| Presentation | One central stream/exit policy for parser/display outcomes; family handlers return their existing typed output/errors through shared presentation adapters |

The immediate jobs integration can therefore be small: root composition adds
the [existing subtree][rust-jobs-parser], `Invocation` gains `Job(JobCommand)`, central conversion
calls the existing jobs converter, and central dispatch selects the existing
`JobHost` adapter. Those changes are a proposal for the front-door owner,
not code added by this document and not proof that all host dependencies exist.

Do not reimplement jobs argument definitions in `main.rs`, pass a stringly typed
bag through the SDK, or create a second job-polling engine. Existing
`JobHost::execute`, `dispatch_json`, and `run_json` provide reusable handler
boundaries. Full text presentation, shared headers, operation reconciliation,
and executable host configuration still need their separately evidenced wiring.

### Policy details that make the boundary usable

1. **Determine family from command structure, not arbitrary values.** Apply a
   lexical adapter only to its selected family. An account, query, file name,
   JSON value, or job argument named `get` is not a read command. Preserve the
   existing auth/read collision regressions and add analogous family cases.
2. **Preserve meaningful states.** Keep absent, present-empty, and present-list
   inputs where the source distinguishes them. Preserve alias spelling when
   output uses it, while dispatch uses canonical kind. Avoid early path
   resolution, lossy argv round-trips, and narrowing Python integers without a
   documented supported range.
3. **Resolve shared options once, deliberately.** The root owner defines output
   override/default policy, help/version precedence, skill presence, and group
   help. Jobs currently declares output/skill on its leaves; that is not a
   license to duplicate root globals blindly. Compare argument placement and
   defaults before adapting registration to a shared root.
4. **Never succeed by ignoring unsupported input.** Reject unsupported flags
   before execution or return the standard typed unsupported result from the
   host. Recognizing GET export syntax is not implementing an export.
5. **Keep parsing pure and non-exiting.** Use `try_get_matches_from` and return
   results. Do not read credentials/configuration, create a runtime, open files
   referenced by arguments, initialize global logging, or call services from a
   value parser. Validate syntax there; do capability/resource validation in
   the host. Help and syntax failures must remain safe offline.
6. **Keep errors secret-safe.** Preserve machine-classifiable errors internally,
   but do not log raw argv, raw Clap diagnostics, passwords, tokens, query
   contents, or arbitrary provider text. Improving error usefulness requires a
   deliberate safe formatting policy, not `error.to_string()` at the binary edge.
7. **Keep one registration source.** Compose family-owned Clap definitions in
   the root. Reuse their schema for help and future completion. Use the Python
   AST inventory as an independent comparison, not a generator that silently
   makes Rust and its oracle agree.

An added public enum variant can affect exhaustive Rust matches, even if it is
conceptually additive. The integration owner must update consumers and document
that source-API change. "Stable contract" here means an agreed ownership/data
boundary for coordinated development, not a claim of semver-stable external
Rust APIs or a stable Rust ABI.

## Happy-path priority and deferrable strictness

Avoid delaying every new working command until every argparse diagnostic is
byte-identical. Equally, do not label valid-but-misparsed input "only an error
message difference."

| Priority | Examples | Treatment |
| --- | --- | --- |
| Required for a working family | Canonical path/aliases in the approved slice, common flags, value counts, expected query/path meaning, explicit host dispatch, output/exit behavior, no ignored options | Deliver with source-backed successful-path consumers; a schema-only registration is insufficient |
| Required safety boundaries | Secret-safe errors, no parse-time service/credential work, cancellation and mutation uncertainty, no fake success or fallback | Not deferred as malformed-input polish |
| Existing measured read behavior | Scalar LS versus GET lists, repeated options, attached values, path boundaries, help/version precedence, auth/read collisions | Preserve existing fixtures and behavior; do not drop already-working compatibility to simplify new wiring |
| Deferrable exact lexical expansion | Unmeasured Unicode decimal classification, obscure argparse inference/ambiguous-prefix edge cases across new families, rare attached/clustered malformed spellings | Track exact source examples and native outcomes separately; stricter rejection can be a documented preview gap |
| Deferrable diagnostic polish | Exact argparse wording, usage wrapping, formatter color/spacing, matching every invalid-input error precedence | Track separately without altering the independent oracle or reporting byte parity |
| Separate valid-use feature work | REPL, batch, custom group help, full aliases, hidden skill/header consumption, terminal completion, output-default placement | Sequence explicitly; these are real missing surfaces, not merely malformed-input differences |

Python's root-only output override being erased by a subparser default is an
example where exact source behavior and a cleaner future UX differ.
Record the compatibility gap and any proposed behavior change separately.
Do not silently change defaults while claiming to translate the source.

For every outstanding comparison, retain source pin, argv/mode, expected
meaning/streams/exit, observed native result, and classification. Keep
source-exact successes, approved exceptions, stricter-invalid-input differences,
unimplemented valid use, and failures distinguishable. An allowed preview
difference remains a difference, not an exact pass.

## Evidence needed before broader claims

The existing raw-query inventory has **60 source cases: 42 accepted and 18
rejected**. Two native probes observe the same cases; that is not 120
independent source cases. Its **16 source consumer cases** produce **23 native
observations** because seven GET cases run in both probes.
Those figures describe this bounded inventory, not the 52-leaf CLI.

Relevant committed evidence includes:

- [`query_arguments.rs`][query-tests]: scalar/list differences, quote handling,
  presence, repeated options, option boundaries, negative numbers, prefixes,
  and help/version precedence through the public parser.
- [`auth_arguments.rs`][auth-tests]: auth/read name collisions, interaction
  policy, and rejection of irrelevant output options.
- [`read_commands.rs`][read-tests]: real SDK/context/projection/output
  composition through injected, offline dependencies; not just field parsing.
- [`cli_contract.rs`][cli-tests]: subprocess stdout/stderr/exit/redaction and
  feature-dependent host behavior with cleared ambient environment.
- The [source raw-argument reference][query-oracle] and
  [command AST inventory][inventory-extractor]: independent source evidence,
  not expected outputs generated from the Rust implementation.

Future family integration needs both a composed-root parse fixture and a
parse-to-real-handler offline consumer, including representative error paths,
without deleting library regressions or rewriting Python expectations.
Library tests establish their exercised contracts, not root registration,
host configuration, mode behavior, live authentication, or entire-CLI parity.

## Decisions now, and decisions that can wait

**Proceeding recommendation:** keep Clap builder, existing DTOs and family
adapters, one central root writer, and small end-to-end family integrations.
Retain the current bounded shim where evidence requires it; extend it only for
a demonstrated Python rule, not into a general homegrown parser stack.

The near-term integration decisions are ownership of root-versus-family
options, exact family DTO/conversion boundaries, and which valid-use surfaces
belong in the next executable slice. They should be agreed at the small
contract level before multiple families depend on them. They do not require a
library replacement or another parser-writing lane.

Defer a builder-to-derive conversion, library switch, generalized registry
generation, exact malformed-input formatter emulation, and completion
implementation until there is a concrete need and bounded acceptance evidence.
REPL/batch remain explicit product work; they are not supplied by any parser
selection.

For preview distribution, a process contract is separate from the Rust API:
binary name/version, argv, encoding, environment/configuration, stdout/stderr,
exit status, cancellation, and output schema are observable. Rust DTOs are
in-process source contracts; they are not a stable DLL/FFI ABI. Nothing here
requires a C ABI, runtime command plugin loader, or changes to the preview
build workflow.

## Sources

Repository links below are pinned to the inspected commits. Library links use
the checked versions; release-metadata links identify the exact version, rather
than a moving latest-version claim.

[rust-pin]: https://github.com/eisber/fabric-cli/commit/9c396cef44b736733a635070431d871c8f18762c
[python-pin]: https://github.com/microsoft/fabric-cli/commit/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4
[ci]: https://github.com/eisber/fabric-cli/actions/runs/40380004702
[rust-parser]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/src/parser.rs
[rust-main]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/src/main.rs
[rust-manifest]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/Cargo.toml
[rust-lock]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/Cargo.lock
[rust-auth-parser]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/src/parser/auth.rs
[rust-session]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/src/session.rs
[rust-jobs-parser]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli-jobs/src/parser.rs
[python-setup]: https://github.com/microsoft/fabric-cli/blob/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4/src/fabric_cli/core/fab_parser_setup.py
[python-main]: https://github.com/microsoft/fabric-cli/blob/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4/src/fabric_cli/main.py
[python-interactive]: https://github.com/microsoft/fabric-cli/blob/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4/src/fabric_cli/core/fab_interactive.py
[python-reads]: https://github.com/microsoft/fabric-cli/blob/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4/src/fabric_cli/parsers/fab_fs_parser.py
[python-globals]: https://github.com/microsoft/fabric-cli/blob/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4/src/fabric_cli/parsers/fab_global_params.py
[python-util]: https://github.com/microsoft/fabric-cli/blob/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4/src/fabric_cli/utils/fab_util.py
[inventory]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/tests/compatibility/commands/inventory.json
[inventory-extractor]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/tests/compatibility/commands/inventory.py
[query-tests]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/tests/query_arguments.rs
[auth-tests]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/tests/auth_arguments.rs
[read-tests]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/tests/read_commands.rs
[cli-tests]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/crates/fabric-cli/tests/cli_contract.rs
[query-oracle]: https://github.com/eisber/fabric-cli/blob/9c396cef44b736733a635070431d871c8f18762c/rust/tests/compatibility/query_arguments/README.md
[clap-docs]: https://docs.rs/clap/4.6.7/clap/
[clap-features]: https://docs.rs/clap/4.6.7/clap/_features/index.html
[clap-derive]: https://docs.rs/clap/4.6.7/clap/_derive/index.html
[clap-complete]: https://docs.rs/clap_complete/4.6.11/clap_complete/
[clap-release]: https://crates.io/api/v1/crates/clap/4.6.7
[bpaf-docs]: https://docs.rs/bpaf/0.9.28/bpaf/
[bpaf-design]: https://docs.rs/bpaf/0.9.28/bpaf/_documentation/_0_intro/index.html
[bpaf-options]: https://docs.rs/bpaf/0.9.28/bpaf/struct.OptionParser.html
[bpaf-release]: https://crates.io/api/v1/crates/bpaf/0.9.28
[argh-docs]: https://docs.rs/argh/0.1.19/argh/
[argh-from-args]: https://docs.rs/argh/0.1.19/argh/trait.FromArgs.html
[argh-info]: https://docs.rs/argh/0.1.19/argh/trait.ArgsInfo.html
[argh-release]: https://crates.io/api/v1/crates/argh/0.1.19
[lexopt-docs]: https://docs.rs/lexopt/0.3.2/lexopt/
[lexopt-project]: https://docs.rs/crate/lexopt/0.3.2
[lexopt-release]: https://crates.io/api/v1/crates/lexopt/0.3.2
[pico-docs]: https://docs.rs/pico-args/0.5.0/pico_args/
[pico-project]: https://docs.rs/crate/pico-args/0.5.0
[pico-release]: https://crates.io/api/v1/crates/pico-args/0.5.0
