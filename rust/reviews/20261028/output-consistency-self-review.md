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
