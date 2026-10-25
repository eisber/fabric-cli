<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded read-command host

`fabric-cli` composes the existing SDK facade, pure Fabric context resolver and
JSON output library. It is not a production authentication or transport host.
The `fab-rs` executable recognizes Python's `ls` (`dir`) and `get` names, paths,
and flags, but returns sanitized `NotConfigured` errors for reads. Ordinary users
cannot run live reads through this binary yet. Help/version need no host.

## Public library boundary

Call `parser::parse_args` with argv[0] included, then pass `Invocation::Read` to
`ReadHost::dispatch` or `ReadHost::run`. The host explicitly supplies:

- A real `fabric_sdk::Client` with caller-injected credentials and selection.
- Workspace, item, and capacity executors. One caller-owned
  `fabric_sdk_http::HttpExecutor` implements all three.
- Immutable current/home context and an already-loaded `Lookup` snapshot.
- Configuration, a validated output timestamp, query projection, and writers.

No runtime, clock, mutable global state, token discovery, Python/az/azd bridge,
HTTP/TLS backend, file export, or production fixture mode is installed.
Cancellation is a caller future. Dropping the operation drops SDK futures;
partial pages never become successful command data. Writers are caller-owned;
partial write failures propagate without fallback, retry, flush or success.

`dispatch` returns the existing `RenderPlan`. `run` writes that plan and returns
its exit status. JSON success and command failure both use stdout; Unauthorized
exits 4, other command failures 1. Cooperative SDK cancellation is a command
failure, not the separate Python main keyboard-interrupt branch (exit 2).

## Implemented composition

Root `ls` lists real SDK workspaces, filters future workspace types at the CLI
boundary, attaches canonical suffixes and stably sorts lowercase full names.
Workspace `ls` lists real SDK items, filters future/non-string item types,
attaches canonical suffixes, and sorts by Python's short-name/enum-name keys.
`--long` adds item IDs; root `--long` uses the existing capacity SDK for
capacity name/ID/region enrichment. Duplicate capacity/workspace IDs use the
last detail row. Missing capacity values retain Python's defaults/nulls.
`--all` and `ReadConfig::show_hidden` expose sorted virtual collection names.

No-query LS works with `NoQuery`, using source column projection. A nonempty
query instead sees sorted source rows before column projection, through
`Projection::project(Value, Option<&str>, bool)`. Call
`ReadHost::with_default_projection()` to select the built-in `PythonQuery`
provider, or supply that provider directly. It uses the real
`fabric-cli-query` library rather than a callback stub.

Item metadata `get` resolves a path using the supplied snapshot and actually
calls the source-specific SDK collection through
`get_item_metadata_from_collection`. Its finite `ItemCollection` maps the
source's exact collection spelling; the CLI preserves its separate 13 rejected
item types before authentication. Existing generic SDK methods remain available
without changing their `/items/{id}` routes.
It passes the validated, ordered parsed HTTP object directly to the projection
boundary, without reserializing the legacy typed `Item`. Python GET with no query discovers JSON property
paths; it does **not** return the object unchanged. The built-in provider maps
absent GET expressions to native path discovery, preserves `"."` identity and
Python scalar stringification, and maps query failures to source-shaped
`InvalidInput` command errors. No-query LS still uses default columns rather
than path discovery. The query library's documented unsupported numeric and
grammar boundaries remain explicit errors. `NoQuery` remains an intentional
host opt-out, rejecting GET discovery and queries instead of faking success.

Integration tests and the offline read probe now use the built-in provider
after real SDK reads; additional inspection-callback and `NoQuery` tests retain
the injected/disabled-provider contracts. The binary still requires a configured
production host, independently of this completed query composition.

## Explicit limits

The first slice is tenant/workspace LS and source-routed item metadata GET, not
the whole Python `get` command. Definitions,
sensitivity-label confirmation, connections, schedules, and environment/mirroring
augmentation are not ported here. Generic metadata is not advertised as a full
item export. Python's unsupported GET item types are rejected.

`--output`, `--verbose`, `--force`, text output, folder-listing configuration,
folder/OneLake/virtual collection commands, workspace GET, ambient home discovery,
and local filesystem fallback fail explicitly. They are never silently ignored.
Recognized binary read options still fail `NotConfigured` before host execution.

Lookup is pure and caller-loaded; this slice does not recreate Python's TTL
cache, cache-miss invalidation/retry or persistent/identity-bound context. Populate
metadata with the SDK outside `Lookup`, then pass a consistent snapshot. Root
LS uses the listing itself for long workspace details rather than making
Python's second workspace-list request; enrichment is a single-list snapshot.
Folder IDs do not affect flat workspace LS when folder listing is disabled;
folder parent loading/presentation belongs to the separate folder slice.

SDK origin, pagination, malformed-response and identifier safety exceptions
remain in force. In particular, foreign continuation URIs fail instead of
being ignored, malformed pages return explicit typed failures, and no SDK
partial progress is printed as a complete result. Metadata GET preserves source
metadata field order, unknown/null values and JSON number types before native
projection. The six former metadata request-URI differences are closed; matching
the helper's requests and output does not establish full GET parity. See the
[source-routing matrix](../../docs/specialized-get-compatibility.md).

See [command compatibility](../../docs/read-command-compatibility.md).
`examples/read_probe.rs` is a test subprocess harness over real SDK calls and an
offline fake executor, not an executable runtime mode, bridge, or live backend.
