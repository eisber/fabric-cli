<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native GET behavior and option support

This slice follows Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, not the current Python checkout.
The metadata-only entry and the explicitly injected definition/output entry have
different capability requirements. Neither establishes live-service acceptance.

Definition reads can return HTTPS `*.analysis.windows.net` cluster-origin
operation Locations. The definition adapter validates that cluster origin (or
the exact selected Fabric origin), operation GUID and exact status/result path,
then constructs every poll and result request on the original selected Fabric
endpoint. It never follows the cluster hostname or broadens credential origins.
Another Fabric endpoint, including public Fabric when MSIT is selected, remains
an error before any follow-up request. Unrelated operations and external
acceptance reconciliation retain their strict origin checks. Initial definition
POSTs use the shared empty-body framing policy.

## Source branch inventory

The inventory below was checked against the Git objects at the full Python pin
above, not inferred from old gap descriptions. Source paths in this table are
relative to `src/fabric_cli/`. `commands/fs/fab_fs_get.py::exec_command`,
`_get_virtual_ws_item` and `_get_virtual_item` select these handlers;
`core/fab_config/command_support.yaml::fs.get` applies the preceding command
support policy.

The native feature composition now uses the existing extended `PathResolver`
with asynchronously hydrated, command-local scopes. It does not add kinds to
`NodeKind`, invent metadata nodes for external targets, or replace the original
metadata-only resolver. Hidden discovery uses the catalog SDK and shared pager;
singletons reuse typed SDK routes and the existing operation session. OneLake
uses the existing properties/directory/shortcut SDKs and HTTP adapter. Each
successful branch rejoins the same projection, output and evidence boundary.

| Python target and exact handler | Native Rust route | Focused `get_host::resource_cases` evidence |
| --- | --- | --- |
| Workspace; `commands/fs/get/fab_fs_get_workspace.py::exec` | Existing `source_get_resolved` workspace metadata/enrichment | Existing `workspace_get` and `get_host` targets; unchanged |
| Supported Item; `commands/fs/get/fab_fs_get_item.py::exec` | Existing specialized item metadata, definition consent/decoding and enrichment | Existing `get_host`, including `metadata_queries_bypass_definition_consent_but_keep_source_enrichment_and_verbose_is_inert`; unchanged |
| Folder; `commands/fs/get/fab_fs_get_folder.py::exec` | Existing folder GET, followed by shared projection/output | Existing `folder_reads` target; unchanged |
| Capacity; `commands/fs/get/fab_fs_get_capacity.py::exec` and `commands/fs/fab_fs.py::{check_fabric_capacity,fill_capacity_args,_get_capacity_id,_search_capacity_id}` | Fabric lookup and F-SKU check; explicit ARM configuration or ordered subscription search; shared ARM 201/202 reconciliation; ARM singleton plus `fabricId` | `capacity_get_checks_f_sku_searches_arm_and_enriches_with_fabric_id`, `capacity_get_rejects_non_fabric_sku_before_any_arm_request`, `capacity_initial_201_202_reconcile_both_arm_headers_and_preserve_original_metadata`, `capacity_arm_collection_reads_reconcile_through_existing_api_pager_and_shared_operation_loop`, `configured_capacity_lookup_failure_falls_back_in_source_order_and_keeps_original_error` |
| Connection; `commands/fs/get/fab_fs_get_connection.py::exec` | Catalog name lookup, typed connection singleton | `hidden_tenant_singletons_use_typed_routes_and_the_existing_query_output_boundary`, `hidden_get_output_writes_the_projected_value_and_keeps_filename_contract` |
| Gateway; `commands/fs/get/fab_fs_get_gateway.py::exec` | Catalog name lookup, typed gateway singleton | `hidden_tenant_singletons_use_typed_routes_and_the_existing_query_output_boundary` |
| Domain; `commands/fs/get/fab_fs_get_domain.py::exec` | Administrator warning, typed domain singleton, paged workspace membership; source-suppressed failures retained | `hidden_domain_enrichment_pages_and_omissions_are_retained_without_partial_success` |
| SparkPool; `commands/fs/get/fab_fs_get_sparkpool.py::exec` | Workspace scope, source `name` lookup, typed Spark pool singleton | `workspace_hidden_singletons_preserve_name_not_display_name_discovery` |
| ManagedPrivateEndpoint; `commands/fs/get/fab_fs_get_managedprivateendpoint.py::exec` | Workspace scope, source `name` lookup, typed endpoint singleton | `workspace_hidden_singletons_preserve_name_not_display_name_discovery` |
| ExternalDataShare; `commands/fs/get/fab_fs_get_externaldatashare.py::exec` | Source item-name/short-share-ID lookup; real workspace/item/share endpoint, not a workspace-wide fabricated route | `external_share_uses_real_parent_item_and_source_short_id_name` |
| OneLake Folder/File/Table/Undefined; `commands/fs/get/fab_fs_get_onelake.py::onelake_resource` | Source root or properties classification, then directory metadata (not file-content download), preserving merged fields and removing capitalized `ContinuationToken` | `onelake_root_metadata_reuses_directory_paging_and_preserves_source_fields`, `onelake_folder_file_and_table_get_metadata_never_downloads_contents` |
| OneLake Shortcut; `commands/fs/get/fab_fs_get_onelake.py::onelake_shortcut` | Storage HEAD for classification; typed Fabric shortcut GET. No following or reading the external target | `shortcut_get_uses_fabric_metadata_not_external_target_or_storage_download` |
| ManagedIdentity | **Not a missing Python GET port.** Absent from `fs.get.supported_items` and `_get_virtual_item` cases; `fab_fs_get_managedidentity.py` contains no implementation. Source command-policy `UnsupportedCommand`, not success-shaped metadata (a direct call bypassing that policy reaches the Python dispatcher's `NotSupported`) | `source_unsupported_virtual_targets_fail_explicitly_without_fake_reads` |
| Tenant, virtual collection/container, local filesystem | Not source GET targets under the command-support policy; explicitly rejected | `source_unsupported_virtual_targets_fail_explicitly_without_fake_reads` |
| Dashboard, PaginatedReport, SQLEndpoint, MirroredWarehouse, Datamart, OrgApp, MetricSet, ApacheAirflowJob, Exploration, RetailDataManager, Healthcaredatasolution, SustainabilityDataSolution, AISkill | Source `fs.get.unsupported_items`, preserved by `unsupported_get`; **not thirteen missing resource ports** | Existing `get_unsupported_types_fail_and_ml_types_use_specialized_metadata_without_definition`; unchanged |

`utils/fab_mem_store.py::{get_spark_pool_id,get_managed_private_endpoint_id,
get_external_data_share_id}` supplies source lookup behavior. External shares
derive their item name with `utils/fab_item_util.py::get_item_name_from_eds_name`
and their display name with `get_external_data_share_name`: the complete item
name, underscore, then the first hyphen-delimited share-ID component. Underscores
in item names must not change the owning item.

Hidden lookup preserves resource-specific source naming. Connections require a
`displayName` field but use the ID when its JSON value is Python-falsey (including
empty string or null). Gateways use the ID only when `displayName` is absent;
a present null is not an ID fallback, and a present empty string does not select
the ID-named gateway. Remaining malformed IDs/names fail explicitly. The HTTP
regressions are
`hidden_connection_falsey_names_and_absent_gateway_names_use_source_specific_id_fallbacks`
and
`hidden_name_fallbacks_do_not_hide_malformed_metadata_or_guess_present_gateway_names`.

`ReadSession::dispatch_get_with_resources` supplies the expanded explicit HTTP
entry without filesystem classification. `dispatch_get_with_resource_storage`
adds the existing local/Lakehouse output adapter and explicit
`GetResourceConfig`. The original `dispatch_get_with_storage` selects the same
resource engine with absent capacity configuration, which uses the source ARM
subscription-search branch. The native binary instead captures saved capacity
defaults in `NativeSettings::get_resources` once at the command boundary and
passes an owned clone through `native_get::Inputs.resource_config`. It selects
`dispatch_get_with_resource_storage` without rereading settings mid-command.
The pure `GetResourceConfig` type is also available as
`get_host::GetResourceConfig` without native features; the resources-module path
remains an alias. The library never discovers configuration implicitly.
Legacy `ReadHost::execute_get`, `ReadSession::execute_get` and
`dispatch_get_clocked` retain their existing core-node contracts; expanded
resource discovery requires the explicit shared HTTP entry.

Typed SDK safety boundaries remain visible: finite scopes/pages/bytes, no
partial listings as complete metadata, no unsafe continuation origins, and
terminal cancellation/identity/transport failures are not swallowed as optional
domain enrichment failures. ManagedIdentity is rejected without acquiring its
unsupported workspace identity metadata. This is an intentional early rejection,
not evidence that Python supports it. A singleton 201 produces the source quiet
no-op where its handler tests for exactly 200; incomplete 202 or malformed
responses fail explicitly. OneLake file GET deliberately lists metadata exactly
like Python; it does not pretend to be a byte-download command.

The native caller injects its captured subscription/resource-group defaults,
preserving the configured fast path rather than always selecting the source
subscription-search fallback. Missing fields remain absent, empty strings retain
source falsey behavior, and present non-string values fail with a sanitized
`SelectionError::InvalidSetting`. No tenant or capacity selector is inferred from
unrelated configuration. The source
`client/fab_api_client.py::{do_request,_handle_azure_async_op,_poll_operation}`
reconciles ARM 201/202 using `Azure-AsyncOperation`/`Location`. The additive
`OperationSession::for_arm` / `execute_arm_get(ArmGetOperation)` now implements
that policy in the **same** finite poll loop as Fabric. It permits only explicit
ARM selection and public `management.azure.com`, while the existing constructors,
Fabric methods, strict links and raw API defaults remain unchanged.

An initial `Azure-AsyncOperation` header wins over `Location`. Its polling JSON
must reach `Succeeded` or `Completed`; source `Failed`/`Cancelled` retains the
original typed error object. With only `Location`, poll HTTP 201/202 continues
and HTTP 200 completes without interpreting the body. Both branches return the
**initial response body** with status 200; terminal Location does not trigger a
Fabric `/result` request. Capacity metadata and `fabricId` therefore come from
the source result, not a status document. Primary singleton requests retain
accepted operation IDs in `GetEvidence` on success/failure; GET never becomes a
possibly mutating operation. Configured lookup service failures are retained
when the source fallback succeeds.

The explicit API client's additive `with_arm_get_reconciliation` selection lets
ARM subscription/capacity collection pages use the same operation session before
the existing pager decodes them. Default `ApiClient::send` still returns raw
statuses and does not poll. New direct SDK tests live in
`fabric-sdk-operations --test arm`; their presence is not execution evidence.

An opted-in ARM page passes the command's cooperative cancellation into its
active operation session. The outer pager does not drop that fetch before it
returns the original `OperationError` and accepted ID. GET keeps the full
`CollectionError`/partial pages and adds the actual operation receipt to
`GetEvidence`; native controlled cancellation still emits its cancellation
record without replay. Ordinary Fabric paging retains its original outer
cancellation priority. The notify-driven native regression
`arm_collection_cancel_drains_active_operation_and_retains_get_handle_and_cancel_record`
covers both a 201 subscription page and a 202 capacity page suspended in polling.

New test cases are handwritten synthetic inputs, not regenerated or modified
protected expectations. Their presence is an inventory, **not a passing-suite
claim**. Full executable and native Linux/macOS/Windows acceptance require
controller-run evidence.

## Metadata-only entry

`ReadSession::dispatch` and `ReadHost::dispatch` retain this bounded option policy.
The explicitly selected native host uses the definition/output entry instead.

| Target/options | Implemented behavior |
| --- | --- |
| Workspace GET with `--force`, `--verbose`, or both | Existing workspace retrieval/enrichment and query, unchanged output |
| Item GET with a metadata-property query and either/both flags | Existing specialized metadata endpoint and query, without definition confirmation or sensitivity warning |
| Item GET without a metadata-property query and either flag | Explicit `NotSupported` after target discovery, before metadata/definition retrieval |
| Any `--output`, including a present empty value | Explicit `NotSupported` before discovery or file access; never silently ignored |
| No options | Existing read path, including its previously documented gaps |

The source metadata check is an exact, case-sensitive root split at the first
dot. Recognized roots are `id`, `type`, `displayName`, `description`,
`workspaceId`, `folderId`, and `properties`. For example, `properties.nested`
is metadata; `properties[0]`, `Properties`, `.`, and an absent/empty query are
not. This is intentionally not an AST-based JMESPath classifier.

The pinned parser describes `--verbose` as showing all JSON properties, but its
value is never consumed by the GET dispatch, handlers, query helper or error UI.
The handlers' separate `verbose=True` parameter controls printing, and
`deep_traversal` controls path inventory. Neither comes from `args.verbose`.
The bounded host preserves that source behavior rather than inventing deeper
traversal or verbose service-error disclosure. `--force` is similarly inert for
workspace and metadata-only item GET, where no definition prompt occurs.

## Definition and projected-output entry

`fabric_cli::session::get_options::is_metadata_property_query` exposes the source
classifier. `ReadSession::execute_get` / `dispatch_get` and
`ReadHost::execute_get` accept an explicit `GetHost` with the caller's existing
operation executor, confirmation/progress UI, optional `TransferFiles`, local
timestamp, newline policy and finite limits. They share one orchestration path.
They do not discover credentials, create a runtime, access an ambient clock or
replace specialized metadata with generic metadata.

`ClockedGetHost` and `ReadSession::dispatch_get_clocked` alternatively accept a
caller-owned asynchronous `GetOutputClock`. The clock runs after projection and
output-directory resolution, never for absent/empty output. Clock failure is a
failure, not permission to substitute a UTC or epoch filename. The caller-owned
`GetEvidence` also retains observed effects if a bounded host drain expires.

Definition-supported items require consent before metadata acquisition, unless
`--force` explicitly selects the warning-only branch. Metadata-property queries
bypass that consent and definition acquisition, but still retrieve connections,
schedules and applicable type-specific metadata. Parsed verbose remains inert.
Definition acquisition uses the shared `read_definition` / operation session,
then `decode_get_definition_value`, then enrichment and the existing query
provider. Successful decoding sets `payloadType` to `DecodeBase64`; it does not
remove that field. Metadata-only acquisition preserves encoded payloads.

The shared synchronous operation policy fetches `/operations/<id>/result` only
when the **terminal polling response** has a nonempty `Location` header.
Without that header it returns the original accepted body with status 200.
An initial response's location alone does not select result retrieval. GET does
not replay the definition POST or install a second polling loop.

GET `--output` writes the **projected result**, even when empty, not definition
parts and not an output envelope. The pinned storage path is
`<output>/<full_name>/get_<local timestamp>`. Local and Lakehouse JSON each receive
exactly one `.json` suffix. `do_output` adds a suffix only to its displayed
message; the storage writer receives the unsuffixed path and appends the actual
suffix. Local output uses pretty Unicode JSON and explicit native newlines;
Lakehouse output uses default ASCII JSON.

For a nested OneLake source name such as `Files/folder`, local GET output creates
the already-validated relative directory hierarchy one confined component at a
time, then writes `Files/folder/get_<timestamp>.json`. It neither flattens the
source path nor passes a slash-containing name to the strict local directory
adapter. Remote output retains its existing classified path handling. The
regression
`nested_onelake_get_output_uses_real_confined_local_directories_and_projected_metadata`
uses the native deferred local worker and actual `LocalDefinitionFileSystem`,
not a fake accepting unsafe directory names.

The shared query provider preserves objects/lists but stringifies ordinary
scalar query results before storage classification: queried zero becomes `"0"`
and receives `.json`; queried false and null become `"False"` and `"None"` and
remain unsuffixed text. An identity query (`.`) preserves the input value's type.
Empty strings/containers are quiet on stdout but still written with nonempty
`--output`; a present empty output selects stdout instead of granting file
authority.

`GetEvidence` retains accepted operation IDs, possible remote effects,
best-effort enrichment failures, definition access denial and completed or
possibly partial file effects. Negative consent is a quiet no-op; EOF/UI errors,
cancellation, codec-domain failures, file failures and failed stream delivery
remain failures. Cancellation is not rollback.

## Native availability

The `native-read`, `headless` and `interactive` source composition selects
`ReadSession::dispatch_get_with_resource_storage` for GET, using its existing SDK session,
HTTP/operation executor and current-thread runtime. Non-GET dispatch remains on
the existing read/job/family paths. Native output reuses the classified
local/Lakehouse adapter; authentication and service failures never authorize
local fallback. The deferred local worker resolves cwd and native tilde inputs
only when local output is selected. Unix current-user tilde uses an absolute
`HOME`; named-user tilde is explicitly unsupported.

Native GET signals cancellation cooperatively and drains for at most two
seconds before the existing bounded runtime shutdown. Typed operation errors,
accepted IDs and completed/possibly partial files remain owned through the final
one-shot stdout/stderr write. A final writer error preserves its original
`io::ErrorKind` and typed `GetWriteFailure`; no retry or rollback is implied.
The original unconfigured/default-host contract remains unchanged.

The new native GET composition tests require a fresh controller-granted run;
earlier metadata/library receipts do not validate this revision. GET identity
binding validates the exact tenant/account/client/provider selection without
initializing navigation or reading cwd. A later non-GET command with the same
identity initializes its actual navigation normally. The isolated Unix
missing-cwd test and injected remote-output tests require execution before
claiming missing-cwd acceptance. Source availability is not a passed native
acceptance gate.
Unsupported binary/wide/nonfinite/surrogate projection domains remain explicit
failures, not silent conversion. Live identity/service and cross-platform
acceptance remain unrun gates, not a GA claim.

## Typed dispatch evidence

The metadata-only `ReadSession` and its `SourceOperationReadSession` wrapper
also expose `dispatch_with_evidence`. The returned `ReadDispatch` contains the
unchanged `TextPlan` and an optional original `CommandError`. A rendered command
failure retains that error directly; callers do not need to infer cancellation
or unexpected-error classification from output text or numeric exit status.
Source-synchronous workspace failures additionally retain their original
`OperationError` in `operation_error`, including its typed cause, accepted
operation ID, exact raw terminal error and uncertain-effects flag. The command
error is projected through the same existing CLI policy; the rendered operation
envelope is unchanged. The original operation is never reconstructed from that
projection. The metadata-only legacy entry uses the same workspace algorithm
without requiring callers to consume the additional evidence.
Success and quiet no-op reads have no command error, and quiet reads retain an
empty successful plan. Rendering failures remain `Err(TextError)`, including any
partial output. Existing `dispatch` methods delegate and return only the plan,
preserving their signatures, stream contents and exit behavior.

This additive metadata-only evidence API does not change SDK request/response
behavior. Native definition GET separately retains its `GetDispatch` rather than
flattening it into this metadata-only result.

## Offline evidence

From `rust/`, the isolated source helper is:

```text
python -I -B clients\cli\app\tests\test_get_options_source.py
```

It executes selected unchanged functions from the exact Git object with explicit
offline dependencies, never imports the product, and reports pinned versus
current-checkout SHA-256 values separately. It observes metadata classification,
confirmation bypass, decline/EOF/force behavior, inert verbose handling,
query/output precedence, writer failures and isolated storage behavior.
It does not replace or regenerate protected fixtures or baselines.

The end-to-end source helper additionally exercises projection before storage,
single-suffix output, decoded payload types and terminal-location reconciliation:

```text
python -I -B clients\cli\app\tests\test_get_host_source.py
```

The Rust integration targets `fabric-cli --test get_options --test get_host` deny
`unnameable_test_items` and cover the real session/direct host, byte-for-byte
option/no-option output, specialized SDK requests, consent, shared reconciliation,
definition decoding, scalar/container projection, output filenames/newlines,
invalid format, cancellation, partial effects, service errors and failed writers.
Rust compilation/execution and independent review remain controller validation
requirements, not evidence supplied by formatting or the Python source helper.
The binary unit-test selector `native_read::get::tests` covers the mounted native
GET with injected offline HTTP, its classified storage, lazy clock, typed
cancellation and final failed writer. Run it for both `headless` and
`native-read`; it does not use live credentials or service operations.

Expanded resource HTTP cases are nested under `get_host::resource_cases` in
`tests/get_host_cases/resources.rs`, inheriting the target's
`deny(unnameable_test_items)`. After the controller grants a build slot:

```text
cargo test -p fabric-cli --features headless --test native_settings --test get_host --test get_options --test read_commands --test read_session --test workspace_get --test folder_reads
cargo test -p fabric-cli --features headless --bin fab-rs native_read::get::tests
cargo test -p fabric-cli --features native-read --bin fab-rs native_read::get::tests
cargo test -p fabric-sdk-operations --test arm --test standalone --test endpoint_context --test path_segments
cargo test -p fabric-cli-resources
cargo check -p fabric-sdk-api -p fabric-sdk-operations -p fabric-sdk-http --all-targets
cargo check -p fabric-cli --features headless --all-targets
```

These are required commands, not receipts. No test/build execution or platform
acceptance is implied by formatting the new code or verifying frozen source.

Capacity snapshot coverage includes
`native_settings::capacity_defaults_preserve_optional_raw_strings_in_an_owned_command_snapshot`,
`capacity_defaults_reject_non_string_types_without_echoing_saved_values`, and
binary selector
`native_read::get::tests::mounted_capacity_get_uses_command_snapshot_defaults_or_source_subscription_fallback`.
The latter passes the captured configuration through the real native GET
composition with synthetic HTTP, verifies the configured two-GET ARM route
versus the source subscription fallback, and changes only the original in-memory
settings map after selection to demonstrate snapshot ownership. It never mutates
process environment or calls an identity provider. The eight direct SDK ARM
cases are in the separate `fabric-sdk-operations --test arm` target, not
`--test standalone`.
