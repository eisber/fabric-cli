<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Typed-domain workspace GET consumers

This is an additive denominator, not an overlay or replacement for the original
70 source cases. The original seven oracle/fixture/documentation files remain
content-pinned by `consumer_reference.py::IMMUTABLE_BASE_BLOBS`.
All expectations were authored and frozen before native comparison.

| Evidence | Distinct source cases | Actual native assertions per probe |
| --- | ---: | --- |
| Original source inventory | 70 | All 70 observed separately: 13 source-equal, 57 different |
| Typed-domain consumer inventory | 58 | 44 source-exact, 12 explicit safety, two unsupported query outcomes |

Both `read_probe` and `folder_read_probe` execute all selected inputs through the
existing raw parser. Thus the typed-domain suite produces 116 native observations
from 58 distinct source cases, not 116 source cases. Comparisons assert the full
ordered request URLs, token scopes, stdout/stderr bytes and exit status.

## Domain and provenance

Source commit is `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; integration base is
`fad3ea7fee3a3e4e66205ffcf34d6b943bb416d7`. The original runner verifies the
complete source tree and complete executed source-file blobs.
The consumer manifest blob is `7e7c3dd435dd241979da16c1cb6c3dcefd679a23`.
The actual original main/dispatcher/handler/API/HTTP/query/output definitions
execute for every consumer case, with synthetic offline collaborators and time.

Nominal primary success bodies contain string `id` and `displayName`. This is
the assigned typed metadata requirement, consistent with the baseline
`fabric-sdk-items::ItemMetadata` validation; it is not a claim that Python or a
preexisting workspace GET model required those fields. Source's permissive
malformed/missing-field behavior remains visible in the original 70 cases.
The malformed-primary consumer is an explicit negative, not a nominal object.

The 44 exact cases exercise ordered unknown/null/numeric/nested metadata,
replacement slots, empty/single/paged collections, every optional 401/403/404
step, malformed optional JSON/missing values, later-page service failure,
queries/inventory/scalars/empty-output semantics, opaque IDs and Unicode.
A first optional 201 page followed by terminal 200 is source-exact.

The 12 safety cases explicitly assert malformed primary, primary 201/204,
primary/Spark continuations, foreign continuation origin, trailing 201/204
collection truncation, malformed optional collection shapes and non-object
Spark metadata. The two unsupported cases retain the existing query library's
wide-sum and oversized-literal boundaries. Safety/unsupported source results
remain independently asserted and are never substituted with native results.
Finite limits, identity/execution failures, cancellation/drop and stream failures
have additional focused SDK/shared-HTTP consumer tests, not fabricated Python
cancellation observations or extra source-parity cases.

## Original inventory is not a passing native gate

All original 70 inputs were also executed unchanged against both probes.
Each probe produced all 70 observations: 13 matched source and 57 differed.
Most differences are primary bodies lacking the mandated string identity
fields; remaining status, continuation and malformed-shape differences are
explicitly documented safety boundaries. Original frozen category and proposed
native-expectation fields are unchanged. No blanket overlay, reclassification,
waiver, input exclusion or native-output-derived expectation was approved.

The task handoff's `original70-native-evidence.json` retains every original
input, Python protocol and both actual native protocols, plus contract references
and probe hashes. Its evidence-only collector is a session artifact, not a
shipped command or a passing parity comparator. Do not advertise 70 exact native
outcomes, or use the 58 consumer cases to erase those original differences.

## Local validation and commands

Windows x64/MSVC local execution passed all 70 original source contracts and 20
original guards, all 58 consumer source contracts and 14 consumer guards, then
the separately classified 58 comparisons through each native probe.
The source-only original runner is intentionally not invoked with a native probe
as an all-passing gate.

From `rust/`, using the actual configured private target and existing isolated
development Python, under the required shared validation mutex:

```powershell
python -B tests\compatibility\workspace_get\reference.py
python -B -m unittest discover -s tests\compatibility\workspace_get -p test_reference.py -v
python -B tests\compatibility\workspace_get\consumer_reference.py
python -B -m unittest discover -s tests\compatibility\workspace_get -p test_consumer_reference.py -v
python -B tests\compatibility\workspace_get\consumer_reference.py --rust-probe C:\tkwg1302814\debug\examples\read_probe.exe
python -B tests\compatibility\workspace_get\consumer_reference.py --rust-probe C:\tkwg1302814\debug\examples\folder_read_probe.exe
```

No restore, live call, production identity/transport configuration, CI success,
release or full migration parity is implied. Parent integration and fresh native
provider CI remain separate gates.
