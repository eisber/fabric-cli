<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# SRC2/SRC3 output consistency self-review

Author: Copilot. Integration base:
`c6eff5287ec1b723abb0e0d30ce547852c785ec6`.
Python authority: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
The October 27 captures and all older oracles remain immutable.

## SRC2: grouped local success names

Before editing the adapter, the completed Ubuntu job `122675675568` in
[baseline run 40646226696](https://github.com/eisber/fabric-cli/actions/runs/40646226696)
failed `config_success_json_preserves_the_frozen_source_group_name`:
actual `command = "config get"`, frozen Python `command = "config"`.
The run's head is the exact integration base above. Its default test runner
stopped at that failure; it is not SRC3 after-fix evidence.

The new typed `LocalCommand::command_name` explicitly groups config leaves.
Only the successful local adapter uses it. `command_path` and the error adapter
are unchanged, preserving complete `config get`/`config set` failure prefixes.
Non-config commands keep their existing names. File execution, configuration
initialization and format selection remain in their original handlers.

The mounted executable regressions retain the original frozen comparison and
exercise all config success leaves in text/JSON, missing-value silence,
settings and context-file preservation, invalid-set non-mutation, existing
invalid-get/corrupt-config failures, and unchanged `desc` naming. Three actual
pinned-source config text/message checks passed before committing; the existing
capture `--check` passed all 38 observations without rewriting them.

Validation in this worktree: local-crate Cargo formatting, direct Rustfmt checks
of changed central CLI files, `git diff --check`, and offline locked no-deps
metadata (32 packages). Workspace-wide Cargo formatting hit Windows error 206
because the long worktree makes its rustfmt command line too long; bounded
formatting succeeded. No local Cargo build, test, Clippy, live authentication
or profile/cache operation was run. Native after-fix execution is an explicit
integration CI gate, not a result claimed here.

Required after-fix selectors from `rust/`:

```text
cargo test --offline --locked -p fabric-cli --test local_mounted
cargo test --offline --locked -p fabric-cli --features native-read --test local_mounted
cargo clippy --offline --locked -p fabric-cli-local --all-targets -- -D warnings
```

## SRC3: GET's post-query conditional output

The preserved `empty_get_projection_is_silent_at_the_actual_command_boundary`
regression was already supplied as a failing mounted case. Before this repair,
the item GET branch unconditionally constructed `Some(OutputInput)` after
projection, making `[]` a success envelope. The combined baseline CI above
stopped at SRC2 before running its native-read audit; it must not be cited as
an additional executed SRC3 failure.

Before implementing the conditional, 18 additional offline source observations
executed complete blob-pinned Python `search` and `query_and_export` definitions
using real Python JMESPath 1.0.1 and private synthetic data:

| Query result before the source search wrapper | Post-query value | Text | JSON |
| --- | --- | --- | --- |
| `[]`, `{}`, `""` | Unchanged | Silent | Silent |
| `false` | `"False"` | `False` plus newline | Success/data `["False"]` |
| Null or missing field | `"None"` | `None` plus newline | Success/data `["None"]` |
| Zero | `"0"` | `0` plus newline | Success/data `["0"]` |
| `"hello"`, `["part"]` | Unchanged | Value plus newline | Success/data retained |

This explicitly avoids checking raw metadata truthiness or interpreting string
`"False"`/`"None"` as falsey. The item GET branch alone now returns `None` when
its completed projection is falsey. The existing workspace GET conditional is
unchanged. No helper coercion, query-engine change, shared serializer change,
authentication, retry or operation-engine change was needed.

The folder wrapper already returns the delegated optional output. Its new
direct-versus-wrapped tests assert identical silence/nonempty bytes, exactly
two independent item GETs, unchanged Fabric scopes and no folder traversal.
The production-session audit retains the original frozen empty capture and
adds both formats for empty objects/strings, scalar stringification, visible
nonempty data, LS/query empty success, job empty-result presentation, typed
query/service failures, no-write silence and propagated writer failures.
Its fake HTTP adapter counts actual requests and selected scopes.

The task's `NoRunsFound` wording is not the adopted implementation's error
contract: the immutable `job-empty` capture, Python `fab_jobs_run_list` and the
current native job host all return Success/message `"No runs found"`. The
additive tests preserve that actual contract; no job source or oracle changed.

Additional required after-fix selectors:

```text
cargo test --offline --locked -p fabric-cli --test folder_reads
cargo test --offline --locked -p fabric-cli --features native-read --test output_contract_audit --test read_session --test job_read_session
cargo clippy --offline --locked -p fabric-cli --all-targets --features native-read -- -D warnings
```

Final bounded checks are formatting of all modified Rust files, offline locked
metadata, unchanged 38-capture `--check`, source observations and diff integrity.
Native after-fix tests/build/Clippy were deliberately not run locally, per the
host restriction; the integration owner must run the selectors in CI before
claiming a passing candidate. No commits were pushed or merged into the root.

The macOS job `122675675518` also exposed a pre-existing, independent
`navigation_persists_only_its_explicit_session_and_pwd_uses_stderr` assertion:
canonical `/private/var/...` versus `/var/...` spelling. This repair leaves that
oracle and navigation behavior unchanged; it is a separate integration issue.
GET export/definitions, broader enrichment/operation epochs, Unicode providers,
format-default composition and full command parity remain outside this repair.
