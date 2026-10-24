<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline Python capacity-list reference

This directory is an independent development-time oracle for
`fab_api_capacity.py::list_capacities`, not a Rust implementation, CLI oracle,
production dependency, live-service recording, or authentication test.
`cases.json` was hand-authored **before** the harness. Its expected observations
are immutable; there is no record, update, accept, or regeneration mode.
All data is synthetic and covered by the repository MIT license.

## Source provenance

The source is `microsoft/fabric-cli` commit
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. Before editing, a read-only Git
comparison confirmed that all eight pinned production files in this checkout
match that commit. `reference.py::SOURCE_SHA256` pins the complete bytes of:

- `src/fabric_cli/client/fab_api_capacity.py`
- `src/fabric_cli/client/fab_api_client.py`
- `src/fabric_cli/client/fab_api_types.py`
- `src/fabric_cli/core/fab_exceptions.py`
- `src/fabric_cli/core/fab_constant.py`
- `src/fabric_cli/errors/common.py`
- `src/fabric_cli/utils/fab_http_polling_utils.py`
- `src/fabric_cli/utils/fab_mem_store.py` (consumer contract reference only)

Hashes use SHA-256 of UTF-8 file bytes with **only CRLF normalized to LF**.
`CASES_SHA256` independently pins `cases.json` under the same normalization.
Initial hashes were calculated with PowerShell `Get-FileHash`, not generated
from oracle observations. Every observation verifies source pins again; changed,
missing, or additional source inputs fail closed. The hard-coded source commit
must also match the fixture. Any source or expectation change requires
independent review, not a digest update to make a Rust implementation pass.

The consumer-contract follow-up added nine new hand-authored cases and explicit
Rust-exception metadata. It did **not** change any of the original 25 response
scripts or expected observations. PowerShell hashing independently confirmed
those original blocks unchanged. The corresponding `CASES_SHA256` update records
only this authorized additive fixture/metadata change; it was not derived from
Rust or oracle execution. The consumer source pin was added at the same time.
A subsequent hand-authored second-page-201 case records a source gap without
changing any prior expected observation.

## Actual executed source

The harness reads and compiles selected AST definitions without rewriting their
bodies. It never imports `fabric_cli`, `requests`, or real identity dependencies.
The actual call chain is:

1. `list_capacities` sets `args.method = "get"` and `args.uri = "capacities"`.
2. `do_request` selects the real default endpoint, version, scope, query encoding,
   credential-call placement, response classification, and four-attempt 429 loop.
3. `_get_session` executes the real shared-session factory against explicit
   session/adapter/retry-constructor doubles.
4. `_handle_successful_response` recursively follows header/body tokens.
5. `ApiResponse.append_text` performs actual JSON page aggregation.
6. `get_polling_interval`, `CommonErrors`, and all four API/CLI exception classes
   execute their original definitions.

The real user-agent builders and private-link selector execute too, with a
synthetic platform/version, an empty environment, and private links disabled.
Selected constant assignments execute from source; endpoint configuration is an
explicit double returning each assignment's default, never host environment
values. Auth and context imports inside `do_request` resolve only to in-memory
doubles through its private `__import__` implementation. No product module is
registered in `sys.modules`.

The case-insensitive header mapping, HTTP responses, retry/adapter constructors,
auth, context, logger/UI sink, clock, platform, and environment are explicit
test doubles. They are not reference implementations of list, paging, errors,
or retry decisions. Standard-library `urlencode` supplies requests' URL-encoding
dependency. The transport consumes precisely the ordered synthetic responses
and checks the sentinel credential. No urllib3 adapter execution is claimed:
502/503/504 adapter retries, connection failures, TLS, and Retry-After HTTP dates
are outside this slice. Existing HTTP compatibility fixtures remain untouched.

## Fixture schema for Rust consumers

The top-level shape is:

```text
{
  schema_version: 1,
  source_commit: "<40 hexadecimal characters>",
  cases: [{
    name: "<unique stable case name>",
    rust_parity: true | false,
    rust_exception?: "<required explanation when rust_parity is false>",
    responses: [{
      status: <HTTP integer>,
      body: <any JSON value> OR raw_body: "<exact text>",
      headers: {"<HTTP name>": "<string>"}
    }],
    expected: {
      value: <raw aggregated value> OR error_code: "<source exception code>",
      response_json?: <entire parsed ApiResponse JSON>,
      total_pages?: <observed source metadata>,
      error_type?: "<Python class name>",
      error_message?: "<structured exception's message attribute>",
      requests: [{method: "get", url: "<absolute URL>"}],
      scopes: [["https://api.fabric.microsoft.com/.default"]],
      sleeps: [<virtual seconds>]
    }
  }]
}
```

`body` and `raw_body` are mutually exclusive, including JSON `null`. JSON bodies
are serialized with standard-library `json.dumps`; raw bodies are unchanged.
Request methods are the actual lower-case source arguments, not wire
normalization. A scope entry represents one `get_access_token` call and retains
the original list-shaped argument. It occurs **once per page invocation, not
once per retry attempt**. Sleeps include zero and the fourth/final 429 delay.
An absent `total_pages` is different from zero; recursion produces **1**, even
after three pages. Rust can compare collection/request/error semantics and
label metadata/safety differences rather than silently changing this source
expectation.

`response_json` is the authoritative successful result. The convenience `value`
field is its `value` member, or null when an object has no such member; a
non-object JSON result is copied unchanged. This projection is harness output,
**not extra source validation**. The raw empty-body case calls actual
`ApiResponse.json()` and reports its `JSONDecodeError` type as the error code;
structured product exceptions retain their actual status-code field and
period-stripped message attribute. Full CLI rendering is not claimed.

Thirty-five cases cover empty/initial/middle/final pages, service order and
duplicates, unknown fields, missing/null/future-typed optional properties,
case/Unicode preservation, header presence precedence (including empty headers),
opaque URL encoding, ignored `continuationUri` and `continuationURI`, and
wrong-case body token keys. Failures cover 401, default/public/legacy 403
precedence, 404 ignoring headers/body, generic structured/non-JSON errors,
second-page failure, 429 recovery/exhaustion, and retry on a later page.

Twenty cases have `rust_parity: true`. Fifteen have `rust_parity: false` and an
explicit `rust_exception`: the Python factory accepts arbitrary rows,
missing/null `value`, and raw null/empty responses without typed model
validation. Original id-only paging/retry cases retain their exact raw inputs
and expectations, but are now Python-only because a typed Rust consumer rejects
their missing `displayName`. Separate `consumer_` cases cover those same network
semantics with valid consumer rows. All 35 cases still execute in both oracle
checks; no raw factory golden is skipped, weakened, or rewritten.

The pinned `fab_mem_store.py::get_capacities` directly indexes both
`cp["displayName"]` and `cp["id"]` when constructing capacity items. The approved
typed Rust boundary therefore requires **string `id` and string `displayName`**
on every row. Rejection of missing/null/non-string values is an explicit typed
consumer exception to the permissive raw factory, not a claim that the factory
validates those properties. A dedicated raw case retains missing/null/numeric/
boolean/array/object `displayName` values as source observations.

All additional fields remain raw JSON. In particular, Rust's `state()`,
`region()`, and `sku()` return `Option<&Value>`: missing means `None`, while
explicit JSON null remains `Some(Value::Null)`. Future numbers, booleans, arrays,
objects, and unknown enum strings must remain unchanged. The new valid-row
case checks those distinctions without relaxing either required string.
The oracle also does not waive Rust's required pagination bounds, cancellation,
deadlines, or safe origins.

### Successful second-page 201: intentional Rust exception

`python_quirk_terminal_second_page_201_is_not_merged` uses valid string `id` and
`displayName` on both pages. The initial 200 page has a token, and the terminal
second page is 201. The actual recursive `do_request` returns that successful
201 response, but `_handle_successful_response` appends it upstream **only if
`_response.status_code == 200`**. Thus the final source result contains only
page one, retains its first token and URI, and has no `total_pages` property,
despite having requested both pages. This is a recorded silent-partial source
gap, not a typed-row failure and not an invitation to reproduce the gap.

Rust intentionally follows the existing workspace SDK acceptance of decoded
200/201 pages and the no-silent-partial safety contract, so it accumulates both
pages. The case is explicitly `rust_parity: false` with that `rust_exception`;
its raw Python expected value must **not** be compared for equality with the
Rust aggregate. Host-owned Rust tests should instead assert the documented
exception (both rows returned) without rewriting this source golden.

## Offline boundary and self-tests

All fixture/source reads happen before source execution. During source
execution, an audit hook denies file reads/writes, directory mutation, network,
process launch, registry and native-library access. Source builtins deny
open/input/printing and allow exactly the two fake auth/context imports.
Neither actual sleep nor a real clock is exposed. Virtual sleeps only append a
number and advance an in-memory clock; logger/UI calls are in-memory no-ops.
Extra/unused responses and even caught audit violations fail the observation.
Nothing writes fixtures, caches, config, tokens, bytecode, or scratch artifacts.
The harness never launches subprocesses or another interpreter.

The self-tests run every case twice, compare both exact JSON observations,
reject twenty independent observation mutations, reject mutations of every
source pin and the fixture, and probe the audit/import/builtin boundaries.
Audit probes emit synthetic events rather than performing forbidden operations.
Unscripted and unused HTTP responses must also fail. These are regression
safeguards for trusted pinned source, **not an OS sandbox for hostile code**.
Use an isolated, trusted Python interpreter.

A separate fixture-label self-test checks that each `rust_parity: true`
successful response contains rows with both required strings, and that every
Python-only case explains its Rust exception. This is a metadata check, not a
substitute implementation of the raw factory or the typed Rust model.

## Execution commands and evidence

No execution was performed when authoring these goldens. The host subsequently
ran both commands on October 24, 2026 under explicit coordinator slot
`capacity-parity-20261024-a`, using existing Python 3.13.15 on Windows x64.
All 35 observations and all nine harness self-tests passed; no pins or
expectations were changed to obtain that result. From the repository root:

```powershell
& <python.exe> -I -B rust\tests\compatibility\capacities\reference.py --check
& <python.exe> -I -B rust\tests\compatibility\capacities\test_capacity_reference.py -v
```

Only the Python standard library is required (Python 3.10 or later). `-I` ignores
ambient Python configuration and `-B` suppresses bytecode writes, including
those outside this directory. There is no installation or dependency restore.
The tests do not execute Rust, existing Python tests, real credentials, real
HTTP, or real sleeps. A missing interpreter, source drift, failing golden, or
failing guard is a blocker to report, not permission to change pinned expected
outputs. Native adapter integration and its validation belong to the host.
