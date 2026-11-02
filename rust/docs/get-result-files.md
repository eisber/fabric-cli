<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# GET result files

This bounded slice translates Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`fab_cmd_get_utils.query_and_export` and `fab_storage.do_output`,
`_validate_json`, `_write_to_local`, and `_write_to_lakehouse`.
It does not replace the independently owned GET definition decoder.

`ReadSession::dispatch_with_result_files` and its source-operation wrapper
accept an explicit `ResultFileHost`. Ordinary sessions/direct hosts still reject
nonempty output-file requests without a file capability. The native host uses
the existing asynchronous transfer worker and Fabric-first export resolver,
not a second filesystem or storage engine. No new command flag is added.

## Source behavior

- Complete retrieval, enrichment and query projection before resolving the
  output destination. Invalid queries never create directories or files.
- The output argument selects an existing directory, not a result filename.
  Fabric destinations must be Lakehouse `Files`. A rejected Fabric destination
  is never silently converted into a local destination.
- Generate `<output>/<target.full_name>/get_YYYYMMDD_HHMMSS`. The caller's
  filename clock is invoked after projection and destination resolution.
- Objects, lists and strings parseable as JSON receive one `.json` suffix.
  Local JSON uses four-space indentation, insertion order, UTF-8 Unicode and
  native text newlines, without a final newline. Lakehouse JSON uses Python's
  default ASCII JSON layout. Other local scalars use Python text spelling
  (`None`, `False`, `True`), not JSON spelling.
- Empty/falsy projected results are still exported. `--output_format` controls
  the completion/error presentation, not the file bytes. Result data is not
  printed on stdout.
- Timestamp collisions truncate/replace the existing file regardless of
  `--force`; force remains definition-confirmation policy, not overwrite policy.
- Emit the grey `Exporting result to '…'...` notice on stderr before writing;
  print `Export completed` only after successful storage completion.
- Preserve the Lakehouse writer's argument-reset quirk: successful remote
  completion has no command field and uses configured output format, ignoring
  the original explicit format. Remote failures retain the original GET format.

## Bounds and remaining acceptance

The existing export file policy supplies the 10 MiB aggregate byte bound,
portable-path checks, existing-directory requirement and rejection of
symlink/reparse/device/network/ADS/case aliases. These are explicit safety
constraints, not a hostile-concurrent-filesystem sandbox.

Cancellation and write errors never replay GET service calls or retry writes.
A directory, truncated file, partial file or in-flight storage work can remain;
cancellation is not rollback. Failure retains any preceding export notice and
does not print completion. Non-string, non-JSON Lakehouse scalars are rejected
before mutation rather than reproducing Python's `len(data)` exception after
the remote file is created. Existing JSON numeric/surrogate limits remain.

The native adapter obtains local time through the CLI-only optional
`time 0.3.55/local-offset` capability. Offset-discovery failure is an explicit
`ClockFailed` command failure without file creation, never a silent UTC
substitution. Library callers inject the clock. Deterministic source/native
tests cover a non-UTC offset whose calendar date differs from UTC.
On Unix, `time` can intentionally refuse local-offset discovery in a
multithreaded process. Such failure must remain actionable and explicit;
no `unsound_local_offset` configuration, unsafe workaround, timezone guess or
UTC fallback is enabled. The native executable host remains Windows-only;
these offline checks do not establish Unix native-host acceptance.

`get_result_files.rs` exercises the actual read session with offline HTTP and
owned project-local filesystem roots. `test_get_result_files_source.py` executes
unchanged functions extracted from the immutable Python commit, without
importing product state. No reference fixture or pin is changed. Live storage,
authentication and cross-platform acceptance are not established by these tests.

## Focused validation

On Windows x64 with Rust 1.99.1, the final focused checks used
`CARGO_BUILD_JOBS=2`, `RUST_TEST_THREADS=2`, the shared target and Cargo
`--offline --locked`. All commands below exited 0:

| Check | Result |
| --- | --- |
| `cargo test -p fabric-cli --test get_result_files --test get_options --test read_session --test source_operations` | 43 passed |
| Same command with `--features native-read --test read_session_unicode` | 51 passed |
| `cargo test -p fabric-cli --features native-read --bin fab-rs native_read::endpoint_tests::get_output` | 2 passed |
| `cargo test -p fabric-cli-output` | 31 passed, including doctest |
| `cargo test -p fabric-cli --test workspace_contract` | 10 passed |
| `cargo clippy -p fabric-cli -p fabric-cli-output --all-targets -- -D warnings` | Passed |
| Same Clippy command with `--features fabric-cli/native-read` | Passed |
| `python -B hosts/cli/fabric-cli/tests/test_get_result_files_source.py` | 4 passed |

Per-file `rustfmt --check` and `git diff --check` also pass. No valid-account
authentication, live request, cross-platform execution, commit or publication
was performed by this lane.

The approved clock feature adds only `num_threads 0.1.7`, checksum
`5c7398b9c8b70908f6371f47ed36737907c87c52af34c268fed0bf0ceb92ead9`.
Lock edges add `fabric-cli → time`, `time → libc + num_threads`, and
`num_threads → libc`. Automated comparison confirms every previously locked
external package version, source and checksum is unchanged.

## Preserved source oddity

The pinned `_write_to_lakehouse` assigns `args = Namespace()` before storage
calls. A synthetic `write_to_storage` call with
`command="get", output_format="text"` and `{"name": "café 🚀"}` therefore
passes only `to_path` to its successful output helper. The source test records
that reset, ASCII request body and exact character-count flush. The native
offline storage test checks create/append/flush success and failure, configured
JSON success despite explicit text, and absence of the success command field.

This affects agents expecting the requested presentation format after remote
output. The port preserves the quirk, not a guessed correction. A separate
improvement could retain the original presentation arguments and use another
namespace for SDK paths; it requires an independent behavior-change decision
and must not alter this slice's source expectations. This entry is available
for coordinator relay to the migration forum; no external post is authorized
by the implementation task.
