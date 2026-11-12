<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal CLI composition

`fabric-cli` builds only `fab-rs`. It supports argument-free `auth login`,
`auth status`, `auth logout`, and raw `api`. Azure CLI 2.54+ is the sole credential
source; sign in using `az login` separately. No Rust command changes Azure CLI's
account, subscription, tenant, login, or logout state.

```text
fab-rs --help
fab-rs --version
fab-rs auth login
fab-rs auth status
fab-rs api 'workspaces?continuationToken=ENCODED_VALUE'
fab-rs api workspaces --method get --query 'text.value'
fab-rs api items --method post --input request.json --async --show-headers
fab-rs auth logout
```

`api` switches are long-only: `--method`, `--audience`, `--input`, `--headers`,
`--query`, `--show-headers`, `--async`. Methods are get/post/delete/put/patch;
audiences are fabric/storage/azure/powerbi. There are no aliases, `--params`,
underscore spellings, or polling timing options. Quote endpoint query strings
and URL-encode their values yourself; valid encoded values are not encoded again.
`--query` filters the full response object locally, not the outgoing URL.

## Host boundary

`fabric_cli::run` connects the real parser, preparation, engine, and renderer to
an injected `Host`, cancellation token, and stdout/stderr. The executable alone
owns Tokio and Ctrl+C. Help/version, including subcommand help, return before
host hooks; the executable handles metadata before even creating its runtime.
API input files, body bounds, endpoint origins, custom headers, and query syntax
are validated before loading selection or constructing a provider/transport.

The CLI passes `fabric_core::ApiRequest`/`ApiResponse` to the injected
`HttpExecutor`. The initial context has a 240-second deadline. Default completion
consumes the actual initial response; `--async` skips completion, returns that
response once, and starts no background work. Poll/result GETs do not carry the
original mutation body or custom request headers.

Only recognized Fabric/ARM operations poll. The engine's fixed polling budget
is 600 seconds with one-second initial exponential backoff; Storage/Power BI
remain raw. This differs deliberately from Python raw API's return-immediately
default. Ctrl+C cancels local work, not the service-side operation.

## Selection and output

Selection is independent of Python auth and Azure CLI caches:

- Unix: `$XDG_CONFIG_HOME/fab-rs/selection.json` when the base is absolute;
  otherwise `$HOME/.config/fab-rs/selection.json`.
- Windows: `%APPDATA%\fab-rs\selection.json`.

Only tenant/principal IDs are persisted, with private files on Unix and atomic
replacement. Invalid or unreadable files fail explicitly. Logout removes only
this selection and is idempotent. Access tokens stay in caller-owned memory.
Refresh rejects tenant/principal drift; subscription-only changes are allowed.

Data uses the Python envelope, ordered keys, four-space JSON, ASCII escaping,
and a trailing newline. Headers appear only with `--show-headers`. Empty and
whitespace bodies become `"(Empty)"`; invalid nonempty JSON is a failure.
JMESPath falsey collection/string projections preserve the original payload;
Python scalar string conversion is retained. Projected lists remain lists,
not an extra nested array. An explicitly empty `--query ''` requests Python's
bounded path inventory, rather than silently disabling the query.

Initial HTTP 4xx/5xx are inspectable response data, including with default
completion, not universal command failures. Credential, malformed response,
transport, and automatic polling failures produce structured JSON failures.
Unauthorized exits 4; other failures exit 1. Parser errors are sanitized rather
than echoing potentially sensitive arguments. Errors may include safe recovery
evidence for accepted operations; they never claim backend rollback.

## Offline evidence and provenance

`app/tests/host.rs` exercises the real parser/host/output with injected effects,
synthetic operation responses, and process-isolated host fixtures. It also
executes the real `fab-rs` for metadata and absent selection with an empty PATH.
Provider tests exercise the real Azure SDK credential with an injected fake
process executor; no ordinary test invokes ambient `az` or a live Fabric API.

The independent Python envelope fixtures and compatibility notes are in
`app/tests/fixtures/`. References are pinned upstream
`5b504195c63ef54c7382ca8e8a9d9759717f0ec5`. The request/input/projection logic
selectively adapts donor
`f8cb97e92eab669785954335e0e7276d30a3377e`'s API/query/output leaves, retaining
their Microsoft/MIT attribution. No facade/resource/workspace-service code is
required. See the Rust workspace instructions for verification commands and
the mandatory heavy-build lease.
