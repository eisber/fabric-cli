<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded Fabric capacity listing

`fabric-sdk-capacities` implements the handwritten read-only Fabric
`GET https://api.fabric.microsoft.com/v1/capacities` operation. It is independent
of the SDK facade, CLI, deployment library, and identity implementation.
It does **not** implement ARM capacity lookup, subscription discovery,
creation, deletion, update, resume, or suspend.

Python reference: `microsoft/fabric-cli` at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`src/fabric_cli/client/fab_api_capacity.py::list_capacities` and the shared
`fab_api_client.py`/`fab_api_types.py` helpers. The independent
[source-executed reference](../../tests/compatibility/capacities/README.md)
pins source digests and hand-authored expectations; existing workspace, item,
and HTTP fixtures are unchanged.

## Public consumer

Supply an `Arc<dyn azure_core::credentials::TokenCredential>` bound to the
explicit `CredentialSelection`, a `CapacityExecutor`, and cancellation.
Construction acquires no token. Each page requests the existing Fabric
`https://api.fabric.microsoft.com/.default` scope from that same provider.
Selection does not configure the credential or verify its claims.

`CapacityClient::new` retains the public Fabric default. The additive
`CapacityClient::with_context(RequestContext)` and facade method share an explicit
endpoint binding with workspace/item/folder reads and the HTTP executor. Initial
and continuation requests stay on the selected host, including daily/MSIT,
without changing the Fabric scope. Validation precedes credentials. This adds
no environment discovery, ARM operations or new HTTP/paging policy.

```rust,no_run
use fabric_sdk_capacities::{
    CapacityClient, CapacityExecutor, CapacityListError, CapacityListLimits,
};

async fn list(
    client: &CapacityClient,
    executor: &dyn CapacityExecutor,
) -> Result<usize, CapacityListError> {
    let result = client.list_capacities(
        executor,
        CapacityListLimits::default(),
        std::future::pending(),
    ).await?;
    Ok(result.value.len())
}
```

`fabric_sdk::Client::list_capacities` delegates to this implementation using the
same credential `Arc`, selection, executor, limits, and cancellation. Public
types are also available at `fabric_sdk::capacities` and
`fabric_sdk::models::Capacity`. `Page` and `FabricServiceError` are the existing
workspace-owned types, not parallel definitions. Response and execution-error
types are aliases of the existing executor wire types.

The optional `fabric-sdk-http::HttpExecutor` implements `CapacityExecutor` by
calling its private shared execution method. It reuses streaming bounds,
overall deadline, GET retries, timer and sanitized failure mapping unchanged.
No transport, TLS backend, global runtime, ambient credential, arbitrary
endpoint, or new external dependency is installed.

## Source behavior and consumers

| Source under `src/fabric_cli` | Contract |
| --- | --- |
| `client/fab_api_capacity.py::list_capacities` | Sets `uri = "capacities"` and `method = "get"` before shared execution. |
| `client/fab_api_client.py::_handle_successful_response` | Header **presence** overrides the body token, even when empty; truthy continuation repeats the original route with an encoded token. Python ignores `continuationUri`. |
| `client/fab_api_types.py::ApiResponse.append_text` | Preserves row order and duplicates. Recursive aggregation leaves `total_pages = 1`, not an actual page count. |
| `utils/fab_mem_store.py::get_capacities` and `core/hiearchy/fab_virtual_workspace_item.py` | Directly index `id` and `displayName`, construct `.Capacity` hierarchy names; SDK does not perform that projection. |
| `commands/config/fab_config_set.py::_set_capacity` | Matches display names case-insensitively above the SDK; SDK does not normalize or select a default capacity. |
| `utils/fab_cmd_ls_utils.py::get_capacities_and_workspaces` and `enrich_workspaces_with_details` | Match IDs and read display name/region; missing values get caller-specific defaults. |
| `commands/fs/ls/fab_fs_ls_capacity.py` | Separately reads SKU/region/state and ARM enrichment, sorts/projects and prints above the SDK. |
| `commands/fs/fab_fs.py::check_fabric_capacity` | Applies the `F\d+` SKU policy above the SDK; raw listing must not filter other SKUs. |

`Capacity` requires only string `id` and `displayName`, as directly consumed by
the hierarchy. No SKU/state/region/admin schema is invented. Optional/future
fields are retained in `additional_fields`; `state()`, `region()`, and `sku()`
return `Option<&serde_json::Value>`. Thus missing is `None`, explicit null is
`Some(Value::Null)`, and unknown state strings or future value shapes survive.
`display_name()` returns the exact required name without projection.
Names, IDs, case, service order and duplicate rows are unchanged. Optional fields
are never silently defaulted to `"Unknown"`, and raw SDK rows are never sorted.

401/403 choose the nonempty `x-ms-public-api-error-code`, then `x-ms-error-code`,
then `Unauthorized`/`Forbidden`; 404 always yields `NotFound`. Other failing
statuses use body `errorCode`, falling back to `UnknownError`. Valid structured
`FabricServiceError` data retains messages, request IDs, details, and unknown
fields as untrusted data. Display text does not echo service/provider text,
URLs, or tokens. A direct custom executor may expose 429 as an HTTP rejection;
the shared adapter applies its established finite read-retry policy first.

## Explicit Rust safety differences

These are existing SDK safety requirements, **not new parity waivers**:

- Defaults are 100 pages, 100,000 rows (including duplicates), and 4,194,304
  bytes per response. Exact terminal bounds succeed; another page or row/byte
  over the bound fails explicitly. Executors must bound response streaming,
  not just check after allocation.
- Active continuation URIs must be HTTPS on the explicitly selected Fabric
  origin/443, no user information or fragment, exact `/v1/capacities` path.
  They are validated but never followed; the selected endpoint and opaque token
  construct the next request. Terminal unused URIs are ignored. Repeated
  tokens fail instead of recursing forever.
- Selected body tokens must be strings or null/absent. A present header
  overrides even a malformed body token. Python can stringify other truthy
  token types and treats other falsy types as terminal; this SDK rejects those
  untyped continuations instead of inventing a contract.
- Invalid pages/rows fail with `InvalidResponse`; no malformed or missing
  `value` becomes a successful empty list. Python's factory can return raw
  invalid rows before downstream consumers fail.
- Only 200/201 complete list pages are accepted. 202/LRO and 204 are explicitly
  unsupported rather than interpreted as list completion.
- Python's recursive helper only appends a follow-up whose final status is 200;
  a terminal 201 page can therefore be silently omitted upstream. The SDK
  accumulates decoded 200/201 pages consistently with the existing SDK contract,
  never reporting a known incomplete traversal as complete.
- Cancellation wins ties, drops pending credential/request work and preserves
  only fully committed pages. Dropping the operation also drops local work.
  No cancellation or later failure converts partial results to success.
- `pages_received` counts committed pages accurately, not Python's recursive
  `total_pages` quirk. Page envelope extras are not a raw-response contract.

Failures expose `CapacityListError { kind, partial }`. A page that would exceed
the row limit is not committed. A decoded page whose active continuation is
invalid, cyclic, or page-limited is committed and its next token retained.
No credentials/request are acquired for the next page after those failures.

No CLI collection, text/table/JSON-envelope, hierarchy/cache, auth, live
service, complete Python HTTP, or cross-platform parity is claimed.

## Validation

From `rust`, only under a named coordinator build slot, with one Cargo job and
a private target directory:

```text
cargo test --offline --locked -p fabric-sdk-capacities -p fabric-sdk -p fabric-sdk-http -- --test-threads=1
cargo test --offline --locked -p fabric-cli --test workspace_contract -- --test-threads=1
cargo clippy --offline --locked -p fabric-sdk-capacities -p fabric-sdk -p fabric-sdk-http --all-targets -- -D warnings
python -I -B tests\compatibility\capacities\reference.py --check
python -I -B tests\compatibility\capacities\test_capacity_reference.py -v
cargo fmt --all -- --check
```

The [capacity matrix](../../docs/capacity-compatibility.md) distinguishes source
observations, SDK assertions, explicit safety differences, and evidence limits.
All 35 Python observations remain frozen. The public shared-HTTP consumer uses
the same scripts for 17 exact row/error/request/scope/wait comparisons plus three
explicit existing safety differences: rejecting an active foreign URI, empty
Retry-After, and noninteger Retry-After. Typed-row fixture labels alone do not
waive those transport/origin requirements. The other 15 Python-only factory
observations remain in the oracle, not silently reclassified as Rust parity.
