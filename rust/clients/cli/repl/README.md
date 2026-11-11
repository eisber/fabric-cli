<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI REPL component

`fabric-cli-repl` is a removable library for interactive command input. It
provides line tokenization, raw aliases, safe prompt display, and shell control
flow. The application owns commands, SDK calls, authentication, navigation
state, output streams, logging, and any async runtime. The library never starts
a shell unless the application explicitly calls `run`.

The default feature set is empty. The core has no production dependencies and
works with caller-provided input. The optional `reedline-editor` feature enables
a Reedline adapter for terminal input.

## Application boundary

Implement `ShellHost`:

- `prompt` returns the complete current display prompt before every input event.
  For example, return `fab:/<current-context>$ `.
- `execute(Vec<String>)` delegates owned tokens to the application's normal
  parser and command host. Return `CommandOutcome::Continue` after handled
  parser errors or nonzero command results, `CommandOutcome::Stop` to end the
  shell, or `Err` for a fatal failure. Command acceptance and recovery policy
  remain application responsibilities.
- `notice` renders fixed messages or the application's current help/version.
  `ShellNotice::message()` supplies the fixed messages where applicable.

Implement `ShellInput` for scripted tests, or explicitly construct
`editor::ReedlineEditor::new(options)` in the feature-enabled terminal entry point,
then call `run(&mut input, &mut host, options)`. Use the same `Options` for the
adapter and loop. The blocking input loop belongs on the application's terminal
thread, not an async worker. Command execution and any caller-owned runtime
remain outside this crate.

`run` returns `StopReason::{ExitCommand, EndOfInput, HostRequested}` or a typed
`RunError` preserving input and host failures. Read errors, malformed lines,
oversized data and failed output callbacks never become successful completion.

Applications can isolate this component behind an optional dependency and a
dedicated entry module. Shared command, batch, output, and SDK components do not
need to depend on it.

## Input and display behavior

Tokenization strips outer Unicode whitespace before splitting words. Only ASCII
space, tab, CR, and LF separate interior tokens; other interior whitespace
remains argument data. Single and double quotes group text, adjacent quoted and
unquoted segments concatenate, and empty quoted arguments are preserved.

Outside quotes, a backslash escapes the next character. Inside single quotes,
backslashes are literal. Inside double quotes, a backslash escapes only another
backslash or a double quote; it remains literal before other characters.
Comments are disabled, so `#` remains literal. There is no shell expansion,
process execution, pipeline, redirection, or substitution support: those
characters remain argument data for the application's command parser.

Raw, case-sensitive aliases are `quit`/`q`/`exit`,
`help`/`h`/`-h`/`--help`, and `version`/`v`/`-v`/`--version`.
Leading or trailing whitespace, quoting, or additional arguments prevents an
alias match. Only the standalone `fab` prefix hint uses stripped input. A
whitespace-only line does not dispatch a command. Ctrl+C requests an exit hint
and continues. Malformed quotes or a trailing escape terminate the loop with a
typed lexical error.

EOF is consumed once and returns `StopReason::EndOfInput`. It does not request
a farewell or diagnostic, invent an exit command, or retry input. The application
owns any EOF presentation. Read errors are distinct from EOF and interrupts.

### Intentional normal-EOF exception in `fab-rs`

In the opt-in `fabric-cli` `repl` build, normal EOF ends the session with exit
status 0 and no additional stdout or stderr diagnostic. This includes Ctrl+D
when Reedline reports `Signal::CtrlD`, which the editor maps to
`InputEvent::EndOfInput`. EOF does not print `Goodbye!`; the exact
`quit`/`q`/`exit` aliases retain their farewell. Ctrl+C still displays the exit
hint and accepts the next input rather than ending the session.

This is an owner-requested UX change from Rust baseline
`701620ba30b0a1309a33ef27998315fff54643f2`, not byte-for-byte Python parity.
The pinned Python source
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` prints
`Error in interactive session: ` for normal EOF. Its independent
`tests/test_source_oracle.py` assertion and `tests/contract.json` reference
remain unchanged. Real editor, history, bounds, host and output failures are
not EOF. Lexical failures retain their existing diagnostic and exit policy.

The scripted application tests check exact session stdout, empty stderr and
process exit status without opening a terminal. Editor tests separately check
the Reedline signal mapping. The existing native acceptance harness uses POSIX
PTYs and explicitly leaves Windows terminal checks unrun; these offline tests
do not establish PowerShell/ConPTY, Linux or macOS terminal-key behavior.
The synthetic EOF case in
`rust/tests/acceptance/native/terminal-inputs.json` uses schema 2 to reject the
old diagnostic on either stream. Its runner also requires actual input delivery,
bounded successful exit, observed raw mode and terminal restoration. Harness
unit checks do not replace native PTY execution; the independent Python oracle
remains unchanged.

Prompt context is plain text, with ANSI/control characters, directional controls
and line separators escaped visibly. Printable Unicode and characters such as
`<`, `>`, and `$` remain literal. The caller's prompt is refreshed each iteration.
The adapter disables ANSI coloring and hints, adds no right prompt or extra
prompt indicator, and configures no application command completer. Editing,
key bindings, multiline input, and history-search interaction use Reedline's
terminal behavior.

## Bounds and terminal requirements

Defaults are 64 KiB per submitted line, 1,024 tokens, 4 KiB per original/escaped
prompt and 128 history entries. `Options` lets callers change those finite
bounds; zero history capacity disables retention. Input, token, and prompt
limits must be nonzero, and the history byte budget must not overflow.
Invalid limits fail before the loop emits a greeting. Both the original and
escaped prompt must fit the prompt limit. Limits reject whole inputs without
truncation.

History uses `FileBackedHistory::new(capacity)`, which is Reedline's memory-only
constructor. It has no history path and never loads, creates or synchronizes a
history file. An additional byte guard bounds each stored line, including lines
Reedline attempts to save before returning input. The retained history payload
is bounded by `history_capacity * max_line_bytes`. Empty lines and consecutive
duplicates are not retained. History failures are reported rather than silently
ignored. SQLite, system clipboard, and external-printer features are not
selected.

Submitted line/history limits are **not a hard allocation bound on Reedline's
live editing, undo, paste or search buffers**. Reedline does not expose such a
bound. Do not treat this editor as a reader for untrusted streaming input.

Both actual stdin and stdout must be terminals. The adapter checks this before
constructing Reedline and again before each read. Redirected input/output returns
`EditorError::UnsupportedTerminal`, with no raw mode, environment-based fallback,
pseudo-interactive prompt, or implicit batch reader. Constructing the adapter
does not read input or enable raw mode.

## Offline validation

Run from the workspace's `rust` directory, with locked dependencies available:

```text
cargo test --offline --locked -p fabric-cli-repl --no-default-features --all-targets
cargo test --offline --locked -p fabric-cli-repl --features reedline-editor --all-targets
cargo clippy --offline --locked -p fabric-cli-repl --all-targets --all-features -- -D warnings
cargo test --offline --locked -p fabric-cli-repl --no-default-features --doc
cargo test --offline --locked -p fabric-cli-repl --features reedline-editor --doc
```

The default test suite integrates fake editor input with a fake application host.
Feature-enabled tests additionally exercise actual Reedline types, signal
mapping, prompt rendering, bounded memory-only history, and rejection of
redirected streams in a self-executed child process. Tests do not open a real
terminal, discover credentials, access user history, or contact services.
Real-terminal editing and rendering require separate terminal acceptance checks;
offline tests do not establish complete terminal UX or cross-platform behavior.
