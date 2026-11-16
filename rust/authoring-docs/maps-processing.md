<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental Maps processing

The default-off `maps` feature adds native `fab-rs maps` processing commands.
It does not build tilesets, render saved Map items, generate PNG files, modify a
Map item, or add GeoIntel processing endpoints.

**This is a private-contract integration, not supported public Fabric v1 Maps
processing.** Native client admission, app-only use, tenant and SKU entitlement,
billing, quota, retention, redistribution terms, and production support remain
unqualified. Offline tests do not establish any of these.

## Commands

Build the native executable with `--features maps,native-read,headless`. The
`maps` Cargo feature does not enable processing by itself. Each online invocation
also requires `--experimental-private-contract`, an existing native broker user
identity, and applicable admission and entitlement. No command starts login,
opens a browser, impersonates Azure CLI, reads its credential cache, or falls
back to another identity or endpoint.

```powershell
fab-rs maps --help
fab-rs maps geocode --address "1000 Fifth Avenue, New York, NY" --top 5 --experimental-private-contract
fab-rs maps geocode --address-line "1000 5th Ave" --locality "New York" --admin-district NY --postal-code 10028 --country-region US --experimental-private-contract
fab-rs maps reverse --coordinates "-73.9632,40.7794" --experimental-private-contract
fab-rs maps current --coordinates "-73.9632,40.7794" --units metric --details --experimental-private-contract
fab-rs maps dailyforecast --coordinates "-73.9632,40.7794" --duration 1 --experimental-private-contract
fab-rs maps dailyhistoricalweather --coordinates "-73.9632,40.7794" --start-date 2026-11-13 --end-date 2026-11-13 --experimental-private-contract
fab-rs maps directions --point "-73.9632,40.7794" --point "-73.959,40.783" --mode driving --traffic none --experimental-private-contract
fab-rs maps matrix --origin "-73.9632,40.7794" --destination "-73.959,40.783" --mode driving --traffic historical --depart-at any --experimental-private-contract
```

These are command syntax examples, **not authorization to run live requests**.
This implementation's delivery task permits only offline mocked validation.

All CLI coordinates use **longitude,latitude**. The weather adapter deliberately
reorders its provider `query` to **latitude,longitude**. GeoJSON and reverse
geocoding retain longitude,latitude.

Geocoding accepts either free text or all five structured address fields.
Results retain every candidate, confidence, matching properties, nulls, and
coordinates. Neither HTTP 200 nor high confidence guarantees an entrance or
rooftop location. The source experiment's high-confidence result was 366 metres
from its reference and failed its 250-metre criterion.

Weather preserves the provider's timestamps, time-zone offsets, units, day and
night records, and sources. Forecast duration 1 means the provider's current
forecast day, not necessarily tomorrow. Supported forecast durations are
1, 5, 10, and 15 days; historical ranges are inclusive and limited to 31 days.

Directions use the 2025-01-01 `optimizeRoute` field: `--traffic none` selects
`fastestWithoutTraffic`; `--traffic live` selects `fastestWithTraffic`. The
provider has no directions `traffic` field in this contract. The default
departure is provider-current time. Matrix uses its distinct `traffic` field,
defaults to `historical` and `departAt: any`, and requires an explicit timestamp
or `now` when selecting live traffic. Explicit timestamps require an RFC3339
offset. Both operations preserve mode, departure, and traffic context in output.
Directions request one route with its path and itinerary.

## Identity and trust boundary

Online execution reuses the existing native `TokenCredential`, runtime, and
native HTTP client. The narrowly gated `maps-private` broker capability adds
only the Power BI issuer scope to an existing silently bound application/account.
It does not change ordinary login scope acquisition or default identity behavior.
App-only, managed-identity, and device-code Maps execution are explicitly
unsupported; selecting one fails without acquiring its credential.

The private sequence is:

1. Acquire `https://analysis.windows.net/powerbi/api/.default` silently.
2. Discover Shared through `https://api.powerbi.com/metadata/cluster`.
3. Validate the returned `backendUrl` as an HTTPS root on a subdomain of
   `analysis.windows.net`, without user information, path, query, fragment, or
   a nondefault port, before forwarding the Entra token.
4. Request `/metadata/v202212/azureMapToken` from that validated root.
5. Send the memory-only SAS in an `Authorization: jwt-sas` header to the fixed
   `https://atlas.microsoft.com` provider origin.

The issuer is a private report-oriented Shared metadata contract with admission
and feature gates. No Map or notebook identifier is supplied. Successful direct
Azure Maps experiments do not establish hidden GeoIntel API support or admission
of the native Fabric CLI application.

Tokens never appear in request URLs, result files, or diagnostics. Provider
error text is withheld. A successful response echoing an acquired credential or
a credential-bearing field is rejected, not written. Native transport disables
redirects and automatic retries. This adapter does not retry or refresh any
request, including throttled GETs and uncertain POSTs.

## Bounds and results

The adapter allows at most three service requests per invocation: discovery,
issuance, and processing. The total deadline is 1 to 120 seconds (default 60),
including native identity setup. Request bodies are limited to 32 KiB and decoded
provider responses to 2 MiB. Discovery and issuer bodies are each limited to
64 KiB. Expanded result rows and terminal output are limited to 8 MiB before
publication. Address fields are limited to 512 UTF-8 bytes, candidate count to 20,
directions to 25 waypoints, and synchronous matrices to 100 cells. All inputs
are validated before credential or network access.

JSON output includes separate operation, API version, private-contract label,
request context, completeness, result count, and untouched provider data.
`--output-format text` uses labelled nested JSON instead of flattening geometry
or temporal values. `--query` uses the existing type-preserving JMESPath
evaluator on `data`; it cannot hide a partial-failure exit status.

Every expected matrix pair has explicit zero-based indices and a status.
Missing pairs have a null status, failed pairs retain their provider status,
and either makes the command fail with exit 3 while retaining the partial data.
Duplicate or out-of-range indices and malformed successful cells are invalid
responses, not successful empty matrices. Exit 2 indicates invalid input or
missing opt-in; identity/provider errors are nonzero, with 401/403 mapped to 4.
POST timeout or transport failure includes uncertainty evidence and never
triggers replay.

Use `--format json|csv|parquet --output <new-file>` for **real result rows**, not
invocation-summary exports. The existing no-clobber export sink and codecs are
reused. Nested result, response metadata, and context columns use logical JSON:
JSON retains native nested values; CSV and Parquet store compact JSON cells,
and Parquet labels those fields with `fabric.tabular.logical_type=json`.
Offsets, geometry, candidates, nulls, and provenance are not flattened away.
CSV follows the common codec's documented null-versus-empty limitations and is
not a typed/lossless scalar format. Use JSON or Parquet for typed results.
Query projection and file export cannot be combined. Empty resultsets retain
their declared schema. Export stdout is a delivery receipt, separate from data.

## Offline validation

`--offline-fixture <regular-json-file>` runs the actual native command against a
bounded local **synthetic provider payload**, without identity/configuration
discovery or network access. Outputs are labelled `offline-synthetic-fixture`.
The original research handoff contained selected observed summaries, not complete
recorded weather or geometry payloads. The fixtures in these tests are synthetic;
they are not presented as recordings or live native qualification.

```powershell
fab-rs maps current --coordinates "-73.9632,40.7794" --offline-fixture .\synthetic-weather.json
fab-rs maps current --coordinates "-73.9632,40.7794" --offline-fixture .\synthetic-weather.json --format parquet --output .\new-weather.parquet
```

The smallest combined deterministic validation uses the pinned Rust toolchain,
an admitted serial local build allocation, and isolated caller-owned caches:

```powershell
cargo test -p fabric-sdk --test maps --jobs 1 -- --test-threads=1
cargo test -p fabric-cli --features maps,native-read,headless --test maps --test maps_binary --jobs 1 -- --test-threads=1
cargo test -p fabric-identity --features windows-broker,maps-private --lib --jobs 1 -- --test-threads=1
cargo check -p fabric-cli --features maps,native-read,headless --all-targets --jobs 1
cargo build -p fabric-cli --features maps,native-read,headless --bin fab-rs --jobs 1
```

For a reusable Windows rebuild, use the commands above from the `rust` directory
at the recorded source commit. Set `RUSTUP_HOME`, `CARGO_HOME`,
`CARGO_TARGET_DIR`, `TEMP`, and `TMP` to the caller's own directories, put the
pinned toolchain on `PATH`, and run `vcvars64.bat` and Cargo in the same
`cmd.exe` process. Use `CARGO_BUILD_JOBS=1`, `RUST_TEST_THREADS=1`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, and
`CARGO_INCREMENTAL=0` to reproduce the delivery profile. Restore missing
dependencies through normal TLS; do not borrow another task's caches.

The owner needs a fresh nonconflicting local allocation and the repository's
shared validation mutex; these commands do not renew an expired agent window.
Retain the actual source/tree, feature/profile, executable size, SHA256, command
exits, and mutex-release receipt. The delivery artifact provides the actual
host-specific tool and executable paths separately from these reusable commands.
