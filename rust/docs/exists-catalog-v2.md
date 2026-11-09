<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# V2 OneLake catalog Exists

The explicit `fab-cli-v2` head adds Lakehouse-owned **catalog** namespace and
table selectors to the [shared Exists operation](exists-v2.md):

```text
fab-cli-v2 exists /workspaces/<workspace-uuid>/items/<lakehouse-uuid>/catalog/namespaces/dbo
fab-cli-v2 exists /workspaces/<workspace-uuid>/items/<lakehouse-uuid>/catalog/namespaces/dbo/tables/Orders
```

These are not physical `Tables/` or DFS directory paths. The selected parent
must return valid Lakehouse metadata with matching IDs/type. CLI parent selectors
are explicitly ID-bound in this slice. The SDK's `CatalogNamespace` and
`CatalogTable` variants also accept parent `MetadataSelector`s and reuse the
existing complete, bounded workspace/root-item name resolver. An inventory miss
remains unknown, not absence; ambiguity, type mismatch and a missing/changing
parent stop before catalog access. Folder scoping and metadata-name comparison
are unchanged. Catalog schema/table spelling is exact, with no case folding.
Friendly catalog paths and non-Lakehouse backends are not supported.

## Contract and authentication

The official [Iceberg metadata API][iceberg] and [request samples][samples]
document the following read-only sequence:

1. `GET https://onelake.table.fabric.microsoft.com/iceberg/v1/config?warehouse=<workspaceId>/<itemId>`
   obtains configuration for exactly the selected parent.
2. The response's `overrides.prefix` (or `defaults.prefix`) supplies the route
   prefix. It is **not assumed to equal the warehouse**. The requested HEAD
   template must appear in the response's `endpoints`.
3. `HEAD /iceberg/v1/<prefix>/namespaces/<schema>` or
   `HEAD /iceberg/v1/<prefix>/namespaces/<schema>/tables/<table>` checks that
   exact configured catalog scope.

OneLake accepts the [Azure Storage audience][overview],
`https://storage.azure.com/.default`, not the Fabric audience. Parent metadata
continues to use Fabric. SDK callers inject `CatalogBackend::new(ApiClient,
EndpointBinding)` through `ExistsHost::with_catalog`; both clients must bind the
same explicit account and tenant. The endpoint must be a OneLake/Storage binding.
The native head reuses `NativeSession`, its existing injected credential,
Storage selection, resource HTTP adapter, transport policies and caller runtime.
The public table hostname is the default; `FAB_RUST_ONELAKE_TABLE_HOST` can
explicitly select another validated Microsoft hostname. Host selection never
changes the Storage audience or fixed `/iceberg` root.

Config properties cannot override credentials, origin, API root or warehouse.
Prefixes are bounded relative sequences of nonempty ASCII unreserved segments;
absolute URLs, empty/dot segments, escaping, controls, query/fragment syntax and
backslashes fail explicitly. Namespace/table components are validated separately
and URL-encoded as single segments. Unsupported prefix syntax is an error,
never a request to another origin.

## Results, capabilities and limitations

| Native observation | Exists result | Meaning |
| --- | --- | --- |
| Advertised exact HEAD returns empty HTTP 204 | `true`, exit 0 | Present in the selected configured catalog view |
| Advertised exact HEAD returns empty HTTP 404 | `false`, exit 0 | Missing in that catalog view, not physical storage absence |
| HTTP 401 / 403, including on config | `null`, exit 1 | Authentication required / denied; never absence |
| Config 404, missing advertised HEAD, 405 / 501 | `null`, exit 1 | Configuration or backend capability not established |
| Malformed config, unsafe prefix, unexpected 2xx | `null`, exit 1 | Invalid response or unsupported completion status |
| Nonempty 204/404 HEAD body, other service/transport failure | `null`, exit 1 | Documented catalog result not established |
| Invalid selector or argument | No probe, exit 2 | Rejected before credentials/HTTP |

OneLake exposes one schema level. For non-schema-enabled items it provides a
synthetic `dbo` catalog namespace. No physical `Tables/dbo` presence is required
or inferred: even `dbo` is checked through the advertised catalog HEAD.

The result is scoped to the **Iceberg catalog view**, not every Delta table in
storage or every other catalog protocol. [Delta-to-Iceberg conversion][iceberg]
can be required to expose Delta tables. Configuration, unsupported-operation,
policy and disabled-conversion errors remain indeterminate; the implementation
does not enable or inspect tenant/workspace settings. A catalog 404 is not a
claim that a Delta directory is missing. Operators must establish the intended
catalog visibility prerequisites before interpreting catalog absence as a
business-level table absence.

Native config and HEAD status/body/allowlisted headers share the existing
bounded evidence container. The SDK and root CLI emit the same canonical JSON,
including native boolean/null and `selectedCatalogTarget` scope. Raw provider
messages, config properties and headers are not automatically serialized or
logged. Parent/config/HEAD share one request count, aggregate retention and
output cap. The same latched caller cancellation/deadline future covers every
credential and exchange; the SDK installs no timer, task, runtime or global
registry. The HTTP executor retains its existing GET-only retry policy; no
HEAD fallback or replay is added.

There are no credential-vending requests, table loads, data reads, SQL fallback,
write probes, settings changes, remote cancellation or mutation preconditions.
Reads are sequential observations, not an atomic snapshot, reservation or
permission grant. No live OneLake/Fabric capability acceptance is claimed.

## Offline evidence

`fabric-sdk`'s `exists_catalog` tests cover changed config prefixes, unsafe
prefixes, advertised capabilities, config failures, HEAD statuses/empty bodies,
malformed responses, synthetic `dbo`, parent visibility/ambiguity/type checks,
bounds, cancellation/deadline and transport uncertainty. `fabric-cli`'s
`exists_catalog` tests share those SDK fixtures for parser/projection/exit/byte
agreement; `v2_exists_binary` checks the actual unconfigured executable without
credential discovery. Impact selection registers these tests alongside existing
metadata Exists consumers. Full native/optional gates and independent review
remain integration obligations, not implied by offline source tests.

[iceberg]: https://learn.microsoft.com/en-us/fabric/onelake/table-apis/iceberg-table-apis-overview
[overview]: https://learn.microsoft.com/en-us/fabric/onelake/table-apis/table-apis-overview
[samples]: https://learn.microsoft.com/en-us/fabric/onelake/table-apis/iceberg-table-apis-get-started
