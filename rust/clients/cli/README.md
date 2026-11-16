<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Minimal CLI composition

`fabric-cli` builds only `fab-rs`. It supports argument-free `auth login`,
`auth status`, `auth logout`, raw `api`, and minimal workspace/item CRUDL.
Azure CLI 2.54+ is the sole credential
source; sign in using `az login` separately. No Rust command changes Azure CLI's
account, subscription, tenant, login, or logout state.

```text
fab-rs --help
fab-rs --version
fab-rs auth login
fab-rs auth status
fab-rs api 'workspaces?continuationToken=ENCODED_VALUE'
fab-rs api workspaces --method get --query "text.value[?displayName=='rustcli']"
fab-rs api items --method post --input request.json --async --show-headers
fab-rs auth logout
```

`api` switches are long-only: `--method`, `--audience`, `--input`, `--headers`,
`--query`, `--show-headers`, `--async`, `--polling-config`. Methods are lowercase
get/post/delete/put/patch; uppercase `POST` is rejected with an `InvalidInput`
diagnostic listing supported values and explaining to use `post` instead.
Invalid methods fail before configuration, authentication, or HTTP effects,
without echoing request arguments. Audiences are fabric/storage/azure/powerbi.
There are no aliases, `--params`,
underscore spellings, or individual timing flags. Quote endpoint query strings
and URL-encode their values yourself; valid encoded values are not encoded again.
Every nonempty `--query` filters the response object placed inside `result.data`,
matching Python raw API: `status_code`, decoded `text`, and optional `headers`.
It does not alter the outgoing URL. Body paths are explicit: `text.value` for
Fabric collections, `text` for an array body, or `text.items` for an object field.
Do not prefix expressions with `result.data`. There is no implicit unwrapping or
bare-field row shortcut. Use `@` or `.` for the whole response object. Headers
are queryable only with `--show-headers`. Empty/missing projections never fall
back to the full response. No recursive traversal or pagination is performed.

## Host boundary

`fabric_cli::run` connects the real parser, preparation, engine, and renderer to
an injected `Host`, cancellation token, and stdout/stderr. The executable alone
owns Tokio and Ctrl+C. Help/version, including subcommand help, return before
host hooks; the executable handles metadata before even creating its runtime.
API input files, body bounds, endpoint origins, custom headers, and query syntax
are validated before loading selection or constructing a provider/transport.

The CLI composes an `HttpExecutor` from injected Azure SDK `TokenCredential`,
transport, and clock boundaries, using shared `fabric_core::ApiRequest`/
`ApiResponse` values. The initial context has a 240-second deadline. Default completion
consumes the actual initial response; `--async` skips completion, returns that
response once, and starts no background work. Poll/result GETs do not carry the
original mutation body or custom request headers.

Recognized Fabric/ARM LROs and Fabric job instances poll; Storage/Power BI remain
raw. Fabric non-job fallback intervals are 2, 7, then 20 seconds repeatedly,
respecting server not-before hints. ARM and jobs use each response's `Retry-After`
or a 10-second fallback, without the staged schedule.
`--polling-config '{"poll_interval":5,"timeout":900}'` accepts two optional positive
numbers in seconds. A fixed `poll_interval` never shortens server hints; `timeout`
bounds the full reconciliation phase, including retries and result retrieval.
Omitted/null timeout means no overall limit; individual status/result requests
still have a 240-second bound. Invalid config fails before host effects.
Use `poll_interval`, not `polling_interval`, and numeric values rather than quoted
strings. Errors identify invalid fields. Both keys can be omitted or set to null.
`--async` cannot be combined with `--polling-config`, including `{}`: async mode
returns only the initial response and does not support polling. This fails before
input loading, identity acquisition, or sending a request.
`max_attempts` and the former `--lro-config` spelling are not supported.
This differs deliberately from Python raw API's
return-immediately default. Ctrl+C cancels local work, not the service-side operation.

Each recognized status GET prints and flushes a readable status line to stderr
while the command runs, including terminal states:
`Job instance status: InProgress` or `Operation status: Succeeded`.
Stdout remains one final machine-readable JSON envelope. Lines contain only
canonical recognized statuses, not URLs, queries, headers, arbitrary body fields,
backend errors, or percentages. Unknown/malformed statuses fail without echoing
their contents. `--async`, raw responses, help and version emit no poll progress.
An unwritable progress stream stops local polling with a safe failure and known
operation recovery; it does not cancel backend work.

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
whitespace bodies become `""` (an approved difference from Python's `"(Empty)"`
marker); invalid nonempty JSON is a failure.
Nonempty JMESPath queries preserve empty results rather than returning the
original payload, a security-driven difference from Python. Projected lists
remain lists; empty objects and strings appear as elements in `result.data`.
Python scalar string conversion is retained. An explicitly empty `--query ''`
bypasses projection, matching Python's CLI truthiness check. The query library's
separate empty-expression API still returns a bounded path inventory.

JSON input files containing `null` send no body. All nonmultipart requests,
including bodyless requests, default to `Content-Type: application/json`;
case-insensitive custom overrides are retained. Decimal integers, including
values outside the `i64`/`u64` ranges, retain their exact digits through input,
plain projection, and output. JSON objects retain all legal keys, including
`$serde_json::private::Number` and escaped spellings of that name; those keys
never act as internal numeric metadata. Unsafe numeric query computations fail explicitly
rather than rounding arbitrary integers. Floating-point tokens use finite
binary64 semantics; overflow is rejected before authentication for request input,
or as malformed response JSON for service output.

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
`app/tests/identity_http.rs` composes that adapter with the real HTTP executor
and an injected pending SDK process to cover cancellation and deadline drops.
These cross-domain composition tests stay at the CLI layer, not inside identity.
`app/tests/polling.rs` checks that status lines are flushed before the next GET,
with clean projected stdout, initial-only async behavior, disclosure canaries,
terminal failures, local cancellation/deadlines, and progress-stream failures.

The independent Python envelope fixtures and compatibility notes are in
`app/tests/fixtures/`. References are pinned upstream
`5b504195c63ef54c7382ca8e8a9d9759717f0ec5`. The request/input/projection logic
selectively adapts donor
`f8cb97e92eab669785954335e0e7276d30a3377e`'s API/query/output leaves, retaining
their Microsoft/MIT attribution. No facade/resource/workspace-service code is
required. See the Rust workspace instructions for verification commands and
the mandatory heavy-build lease.
