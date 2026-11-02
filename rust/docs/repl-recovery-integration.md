<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Selective integration of the recovered REPL

The November 2, 2026 integration recovers the original Reedline candidate from
local Git forensic commit `9e2208fbf437ba68ae163cebbc1991501414b445`, whose parent
is `558d0dda718328b760059fa7a5bf55e7491c2006`. It adapts the candidate to the
coordinated tree based on `336cbfcb547807c2176159bb7043a59f2f47c253`, without
cherry-picking the mixed draft. Python behavior remains pinned to
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` in the recovered independent oracle.

## Preserved source

All nine files under `hosts/repl/fabric-cli-repl/` are recovered byte-for-byte:
the manifest, README, pure loop, editor adapter, JSON fixture, three Rust test
files and Python oracle. The crate remains removable and has no production
dependencies with its empty default feature set. `reedline-editor` selects
exactly `reedline = 0.52.1`, with Reedline default features disabled.

The byte comparison applies to the recovered working files before publication.
Git applies the repository's LF text normalization when staging these files.
The forensic commit and recovery archive retain the original bytes; published
text normalization is not a match against the original raw-byte digests.

The complete source-recovery manifest SHA-256 supplied with the handoff is
`D334B470ED21FEC35D5B02C055717AD5D65A51451686DB16DD7536D85A8B79E8`.
The historical candidate manifest comparison (`977DA...`) remains **unknown**:
the old manifest is unavailable. Neither recovering the source nor passing new
checks establishes a match with that unavailable manifest. Historical Windows
receipts in `rust/release/20261030/repl-component.json` are not combined-tree
validation evidence.

## Explicit integration adaptations

- The application retains the original binary-only `repl_host.rs` loop,
  notices, lexical-failure/EOF handling and host tests. Imports, dispatch and
  state wiring are adapted to the current executable.
- Only the interactive state subset of the draft `BatchState` is included in
  `repl_state.rs`. Existing command-line and batch dispatch are not replaced.
  No remote-batch, headless-identity, update or unrelated dependency draft is
  imported.
- Application feature `repl` remains default-disabled. Only a no-argument
  feature-enabled invocation starts the editor. Redirected stdin or stdout
  fails before editor construction, configuration or history access.
- Local interactive navigation uses `command_line = false`; it does not restore
  a command-line context file and never persists navigation. An explicit
  `FAB_RUST_CONTEXT` is validated but cannot resolve remote metadata through
  the local-only host. History stays bounded and memory-only.
- The forensic shell delegates native/authentication commands and batch
  invocations to shared dispatch; it does not refuse them. Its mixed draft main
  expects state-taking typed native handlers, while the inherited native/auth
  files still expose incompatible `ExitCode` handlers. The captured draft is not
  an integration-ready native build.
- The bounded parity repair supplies those missing seams: caller-owned native
  interactive context, runtime-mode propagation, and typed native/auth outcomes.
  Additive `ReadDispatch`/`JobDispatch` result wrappers retain failures before
  rendering while the existing plan-only methods keep their contracts. Source
  GET failures returned as `OutputInput` retain their structured error-code field,
  not a code parsed from output bytes or messages. Ordinary returned exit 2 is
  not inferred to be cancellation or a terminal failure.
- Interactive native navigation reuses the caller's state with persistence
  disabled, updates the next prompt after navigation, and never restores a
  command-line context file. The same existing broker, endpoint, transport,
  runtime/cancellation and request policies are reused. Auth changes only retain
  control-flow outcomes before writing the unchanged result; acquisition,
  cache, scopes and interaction policy are unchanged and still require their
  separate team review.
- Native, authentication and nested batch shell dispatch are preserved. The
  first minimal integration's added refusals were rejected and removed; they
  are not recovered source behavior. Standalone command-line/batch output,
  exit and context policies remain unchanged. Generic family metadata failures
  retain the old `Local` evidence for command-line callers; interactive callers
  receive the recovered typed `CommandFailure` evidence.
- The native executable creates one shared interruption signal. Physical
  copy/move passes that same signal to the existing handler and awaits its
  structured cancellation/cleanup result instead of dropping the handler in
  the outer biased signal branch. Active paths and partial write/delete
  receipts survive through rendering and runtime shutdown. The same
  cooperative path applies to a physical-transfer watchdog interruption;
  signal-registration failure still fails before execution starts. Other
  command interruption behavior is unchanged. Executable-wrapper regression
  tests exercise both physical verbs with delayed cleanup and retained
  `TreeError` evidence; they do not send real OS signals or contact services.
- Executable `deploy` keeps the coordinated early installed-Python backend
  selection, before any native host dispatch. The shell adds no authentication,
  SDK or file-command fallback.

The root-unknown-command exit 2, contained registered-subparser misuse, exact
raw aliases, malformed quote/escape single diagnostic with normal status, and
EOF diagnostic without farewell are preserved. No new completion vocabulary,
disk history, shell expansion or forced persistence is introduced.

## Dependency and acceptance boundaries

The resolver adds 31 registry packages for the editor. Every newly added
registry package's version, checksum and lock dependencies match the forensic
lock entry. Key selections are Reedline 0.52.1, Crossterm 0.29.0, Chrono 0.4.45,
and Unicode segmentation/width 1.13.3/0.2.3. Chrono is editor-transitive, not a new
workspace clock policy. The approved GET `time` 0.3.55/local-offset and
`num_threads` 0.1.7 selection remains intact; no existing registry version is
removed or upgraded. SDK/default CLI dependency graphs must exclude
Reedline/Crossterm; pure component graphs must also exclude SDK and identity.

Run the component's documented pure/editor checks, application default/repl and
native-read/repl all-target checks, architecture/feature-isolation contracts,
and affected local/batch/argv/deployment contracts under the coordinator's
exclusive validation slot. The Python oracle executes immutable Git source
offline and does not rewrite fixtures. CI declares these checks; declaration is
not execution evidence.

Real terminal editing, raw-mode restoration, history search, cursor rendering,
non-Windows terminal execution, actual terminal/native-shell operation and live service
acceptance remain separate gates. Offline fake input and redirected child
processes do not prove them.

## November 2 combined-tree validation

Actual Windows x64 checks used Rust 1.99.1, two build jobs and two test threads,
with offline locked dependencies and the coordinator's exclusive slot:

- Combined native/REPL and default CLI all-target compilation passed.
- Original component pure/editor tests passed (12/19 respectively); both
  documentation-test configurations passed. The pinned Python oracle passed
  all seven cases without fixture changes.
- Default CLI and REPL-enabled host/argv/batch/deployment/context/read/GET/
  Unicode contracts passed. Architecture and editor-feature isolation passed.
- Native/REPL executable tests passed 38 of 39, including typed auth outcomes,
  recovered host/state behavior and both physical cancellation-wrapper
  regressions. The existing
  `native_get_option_gate_defers_metadata_flags_but_rejects_file_output` test
  failed: it expects file-output rejection although the approved GET host
  accepts it (`unwrap_err()` received `Ok(())`). This unowned expectation was
  not changed or skipped; the aggregate native gate remains **failed**.
- Separately selected native downstream suites passed, including family/file,
  GET, Unicode and deployment contracts. The current native-file suite was
  rerun after a peer test-source change and passed all 29 cases.
- Strict all-target Clippy passed for default CLI, REPL-only CLI/component,
  native/REPL CLI/component/local, and the pure component.

The initial all-target compile caught a read/job outcome-field wiring typo;
it was repaired and the combined all-target check passed before tests.
Actual command/exits, logs and before/after raw source hashes are retained in
the integration session artifacts. No source recovery receipt is reused as
current validation, and no independent review or terminal/live acceptance is
claimed. GET-owner resolution and rerunning the failed aggregate native gate
remain required before acceptance.
