<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Optional in-process TOM boundary

This SDK-layer leaf contains the only native hosting boundary for offline
semantic-model BPA. Its safe Rust API is synchronous and does not launch a CLI,
Python, PowerShell, `dotnet`, or another process during analysis. It never
connects to Fabric/XMLA, discovers credentials, or changes a model.

## Features and build

- Default features are empty. The build script exits before inspecting managed
  sources or invoking an SDK; `netcorehost` is not selected. `Analyzer::new` and
  `HostOptions::bundled` return `BindingError::FeatureDisabled`.
- Explicit `dotnet` selects `netcorehost = 0.22.0`, without its default features,
  with only `net8_0`. It dynamically loads an explicitly supplied hostfxr library,
  not `nethost`, avoiding nethost downloads, linkage and ambient host discovery.
- Feature-on builds publish `managed/Fabric.SemanticModel.Managed.csproj` under
  Cargo's `OUT_DIR`. A .NET SDK `8.0.425` (latest patch in that feature band) is
  selected by `managed/global.json`. `FABRIC_BPA_DOTNET_BUILD_HOST` can explicitly
  select the build-time dotnet executable. There is no runtime subprocess fallback.
- TOM is pinned to public NuGet `Microsoft.AnalysisServices [19.118.3]`, which
  publishes net8.0 assets. Verified against the [NuGet package page](
  https://www.nuget.org/packages/Microsoft.AnalysisServices/19.118.3) and the
  [TOM DeserializeDatabase reference](
  https://learn.microsoft.com/dotnet/api/microsoft.analysisservices.tabular.jsonserializer.deserializedatabase).
  Feature-on builds require the workspace's explicit public-only `NuGet.Config`
  and a reviewed managed `dependencies.lock.json`. The build subprocess starts
  with a cleared environment, forwards only OS/SDK essentials, and disables
  NuGet 6.x credential-plugin discovery. It does not consult ambient feed
  credentials, fallback sources, user NuGet configuration or SDK startup hooks.
- Ship the entire managed publish directory, including runtimeconfig/deps JSON
  and dependency DLLs. `HostOptions::bundled` is a development convenience pointing
  at Cargo's build output, not a portable installation path.
- Cross-compiling the Rust host/managed deployment and non-Windows execution
  require separate platform validation; offline Windows tests are not evidence
  of full cross-platform or live model acceptance.

The workspace owner centralizes `netcorehost = { version = "=0.22.0",
default-features = false, features = ["net8_0"] }`. This crate inherits that entry.
It must remain outside default member/consumer graphs and be selected by an
optional dependency and explicit feature in consumers. Its private interop module
is the sole local exception to `unsafe_code = "deny"`; existing workspace crates
retain their safety policies.

### Isolated restore and lock bootstrap

The workspace owner supplies `rust/NuGet.Config` with explicit `<clear />` for
package sources and fallback folders, one `nuget.org` v3 source, and package source
mapping of `*` to that source. The build always supplies its absolute
`RestoreConfigFile`, so configuration does not merge with user/machine files.
Do not add private feeds, credential entries, source fallbacks, TLS-validation
exceptions, audit suppression or ignored-source-failure flags.

The component uses `managed/dependencies.lock.json`; the separate boundary test
executable uses `managed/boundary.dependencies.lock.json`. Bootstrap each once
using an authorized isolated `dotnet restore` with `-p:RestoreLockedMode=false`;
review and retain complete transitive versions and content hashes. Subsequent
builds use locked mode and fail on missing/drifted locks. No guessed or
hand-authored package hashes are accepted. The initial bootstrap is intentionally
pending a build-slot grant; until the component lock exists a feature-on Cargo
build fails before starting dotnet.

The build script confines intermediate files, ordinary build output, publish
output, package/cache directories, SDK home and scratch files to Cargo `OUT_DIR`.
The managed projects exclude source-tree `obj`/`bin` files so a later isolated
build cannot accidentally glob generated sources from an earlier direct build.
SDK-dependent tests must use the same explicit config, isolated directories and
sanitized child-process environment; running them directly from an ambient
developer shell is not evidence that credential/plugin isolation was exercised.
NuGet 6.x's explicit empty plugin list (`NUGET_NETCORE_PLUGIN_PATHS=;` and
`NUGET_PLUGIN_PATHS=;`) avoids its default user-plugin search. This behavior is
specific to the pinned .NET 8 SDK, not a promise about later NuGet versions.

## Public Rust API

```rust,ignore
use fabric_semantic_model_dotnet::{Analyzer, CancellationToken, HostOptions};

let options = HostOptions::new(trusted_hostfxr_library, trusted_publish_directory);
let analyzer = Analyzer::new(options)?;
let cancellation = CancellationToken::new();
let response_json = analyzer.analyze_json(request_json, &cancellation)?;
```

`validate_request_json(&[u8])` validates transport without loading .NET.
`BindingError` is a sanitized, non-exhaustive typed error; model/evidence and raw
managed exception text never appear in it. Code locations are caller-injected
trusted executable inputs, never taken from an analysis request.

## Transport version 1

The request is a UTF-8 JSON object with exactly these five required fields:

```json
{
  "schemaVersion": "1",
  "format": "bim",
  "model": "{\"name\":\"offline\",\"compatibilityLevel\":1600,\"model\":{}}",
  "options": {},
  "evidence": {}
}
```

`model` is a serialized standalone TOM database (BIM), not a filesystem path,
TMSL create/alter command or connection string. TOM loads a real
`Microsoft.AnalysisServices.Tabular.Database` with `JsonSerializer.DeserializeDatabase`
before calling `Fabric.SemanticModel.Bpa.BpaEngine.Analyze(database, options, evidence)`.
The sibling `managed/Bpa` implementation owns rules/evidence and preserves
unavailable/error coverage instead of treating absent evidence as a pass.
The source pin is `microsoft/semantic-link-labs@2ddf46109c1209ceffa88149f4631e7b2275f9a1`.

`format: "tmdl"` is explicitly unsupported in this initial boundary. No folder
parts shape, lossy conversion, filesystem traversal or invented serializer is
accepted. Other versions/formats fail before native invocation. Unknown or
duplicate root fields and non-object options/evidence fail. Request and response
are each limited to 16 MiB. JSON nesting is bounded (128); malformed BIM has a
distinct error. Output must contain schemaVersion, source, status, findings,
rules and coverage with their contract types.

## Lifetime, memory, threading and cancellation

The first successful canonical runtime/component pair is retained in a
process-global strong reference. The loaded hostfxr library, delegate loader and
managed entrypoint remain alive after all public handles are dropped. A different
pair is rejected rather than loading incompatible replacements; failed attempts
are retryable. .NET itself is not unloaded. Callers must not replace the trusted
DLLs in place while the process runs.

The private native ABI uses hostfxr's `ComponentEntryPoint(IntPtr, int)` and a
sequential structure containing ABI version, request pointer/length, response
pointer/capacity and response length. Native pointer size/alignment is shared
with .NET. Rust owns both buffers; managed code copies the request, checks lengths,
writes at most the response capacity, and retains no pointers. There is no
cross-allocator free function, leaked response allocation, or raw-pointer public API.
Ordinary managed exceptions are caught at the outermost entrypoint and mapped
to numeric status without text. Process-fatal CLR failures (for example stack
overflow, access violation or fail-fast) cannot be recovered in an in-process
host; this boundary is not a sandbox for untrusted executable components.

All handles serialize execution through one process gate to bound concurrent
response buffers and avoid evaluator reentrancy. Runtime initialization is also
serialized. A cloneable `CancellationToken` is checked before the gate, after it,
and after evaluation. In-flight TOM/evaluator work is not forcibly interrupted;
its result is discarded after completion if cancellation was requested. No
hard deadline, interruptible waiting or rollback is claimed. Async SDK/CLI callers
must schedule this synchronous CPU work on an appropriate blocking executor.

## Targeted validation

From `rust`, with the coordinator's serialized build slot:

```powershell
cargo test -p fabric-semantic-model-dotnet --no-default-features --test bindings_contract
$env:FABRIC_BPA_TEST_HOSTFXR = '<absolute trusted hostfxr library path>'
cargo test -p fabric-semantic-model-dotnet --features dotnet --test bindings_contract --test bindings_hosted
dotnet run --project capabilities\semantic-model\fabric-semantic-model-dotnet\managed\BoundaryContractTests.csproj --configuration Release
```

The dotnet command is the test selector, not a complete isolation wrapper: apply
the preceding restore/environment/output requirements before invoking it.

Hosted tests require the explicit runtime path and fail, rather than silently
skip, if it is missing. They load real TOM, check repeated calls and handles,
drop/reacquisition, cross-thread serialization, malformed model conversion,
cooperative cancellation and coverage shape.
Transport tests also cover disabled features, request limits, duplicates,
schema/version rejection and unsupported TMDL. No live credentials are needed.
The separate managed boundary test executable compiles the production entrypoint
with a deterministic evaluator test double (never the shipped rule evaluator);
it checks managed exceptions, allocation ownership, response limits, invalid ABI,
real TOM model parsing and repeated calls without needing native Rust hosting.
Run it from the `managed` directory to apply its pinned SDK selection.
