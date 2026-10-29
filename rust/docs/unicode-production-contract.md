<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Production Unicode provider contract and validation evidence

Inspected source/integration checkpoint: `b341071cdf82a0581e998c28bf019068dbf5f94e`.
The shared HEAD advanced concurrently to
`d7391ed18315bc073a209bd408e02e7e20f504cd` by the final small checks; this lane
did not commit, stage, reset, change branches, or edit the controller's files.
Python source: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
This contract records the approved production-provider design, source-version
compatibility and actual validation evidence. The library now exports the
provider and selects its default callbacks after complete corpus generation and
verification. Native central-host integration, independent review and broader
production acceptance remain separate gates. This is not a full-CLI/GA claim.
This lane did not edit manifests, locks, old oracles, commands or peer files.

## Approved exact dependency — controller-owned manifest changes

Add centrally:

```toml
icu_properties = { version = "=2.4.0", default-features = false, features = ["compiled_data"] }
```

Add only `icu_properties.workspace = true` to `fabric-cli-output` dependencies.

The existing lock already resolves **icu_properties 2.4.0**, checksum
`2ea83d991c56158f7cf898bf86436c1dfe7c07ee82fe88951f2695c9f37a379f`,
and **icu_properties_data 2.4.0**, checksum
`b98f29af82d02b163fe50161388350e743bb3107cc8e8c2f7314cb505faddfcf`.
Do not add 2.1.1 or a parallel ICU graph. Their MSRV is Rust 1.88; the workspace
pins 1.99.1. Both crates are `Unicode-3.0`, not MIT.

**Additional graph:** one direct edge
`fabric-cli-output -> icu_properties 2.4.0`; its `compiled_data` feature selects
the already-resolved `icu_properties_data 2.4.0` and `icu_provider/baked`.
The existing `url -> idna -> idna_adapter 1.2.2` graph already selects
`icu_properties/compiled_data`. No new package, alternate version, runtime,
native library, build tool, serde feature, segmenter or casing dependency is
expected. Existing transitive edges are `displaydoc`, `icu_collections 2.4.0`,
`icu_locale_core 2.4.0`, `icu_provider 2.4.0`, `zerotrie`, and `zerovec`.
The output package's lock dependency list will change; preserve all registry
versions, checksums and other edges. This is a manifest/source inspection result,
**not** an executed resolver or `cargo tree` result.

Primary API/data evidence, checked against cached exact crate sources:

- [ICU4X 2.4 property API](https://docs.rs/icu_properties/2.4.0/icu_properties/):
  `CodePointMapData::<T>::new().get(char)` and
  `CodePointSetData::new::<T>().contains(char)` use `compiled_data`.
- [EastAsianWidth](https://docs.rs/icu_properties/2.4.0/icu_properties/props/struct.EastAsianWidth.html):
  `Wide` / `Fullwidth` are property values, not terminal-width rules.
- [GeneralCategory](https://docs.rs/icu_properties/2.4.0/icu_properties/props/enum.GeneralCategory.html):
  individual categories, not Python `isalnum` or `isprintable` by themselves.
- [DefaultIgnorableCodePoint](https://docs.rs/icu_properties/2.4.0/icu_properties/props/struct.DefaultIgnorableCodePoint.html):
  default rendering classification, not a blanket source width-zero rule.
- [GraphemeClusterBreak](https://docs.rs/icu_properties/2.4.0/icu_properties/props/struct.GraphemeClusterBreak.html):
  property data is not segmentation; ICU recommends its segmenter for that.
- [Exact compiled data provenance](https://docs.rs/icu_properties_data/2.4.0/icu_properties_data/):
  **Unicode 18.0.0, CLDR 49.0.0**, LSTM v0.1.0, also recorded in the
  cached crate manifest and README. Do not describe this as UCD 15.1 or 17.
- [Unicode license](https://www.unicode.org/license.txt);
  exact ICU notice and Rich's MIT notice are in `THIRD_PARTY_NOTICES.txt`.

The controller approved and applied this exact dependency direction and retains
workspace/output manifest and lock ownership. No dependency install, resolution,
Cargo build, or global tool installation was needed for the initial proposal.
The authorized subsequent validation and isolated missing-Rich installation are
recorded below; all three manifest/lock SHA-256 values were unchanged throughout.

## Three separate data contracts, not an ICU approximation

The actual current output trait is **`text::CharacterWidth`**. There is no
`TextWidth` or `UnicodeProperties` trait at this checkpoint. The provider
implements the existing trait, and exposes typed source/current property APIs.

1. `fab_ui._char_visual_width` and `_get_visual_length` use CPython
   `unicodedata.east_asian_width`: F/W means 2, **everything else 1**.
   Combining marks, controls, ZWJ and default ignorables are not subtracted.
   Existing scalar wrapping must not be changed into grapheme wrapping.
2. Python `str.title`, `str.isalnum`, `str.isupper`, `str.isspace` and nested
   `repr` use the pinned interpreter's UCD and full casing expansions.
   The generated table records every scalar's actual predicates, categories,
   EAW class, lower/title expansions and contextual-sigma dependencies.
   `title_key` implements CPython's original-input Cased state and final-sigma
   lookahead in linear time; it does not use Rust's differently versioned case
   tables. Source key validation and special replacements remain unchanged.
3. Optional `rich_text_width` implements **Rich 14.3.3, explicit UCD 17.0.0**
   scalar data and `cell_len` sequence semantics. Rich's actual algorithm skips
   the scalar following ZWJ and applies its narrow-to-wide set for VS16, including
   malformed sequences and zero-width intervening scalars. It is not generic
   extended-grapheme segmentation. No `unicode-width` substitution is proposed.

Primary Python/Rich implementation references:

- Source functions above and `_format_key_to_convert_to_title_case` are loaded
  by the existing isolated `TextReference`; its original source hashes remain
  unchanged. `source-data.json` never accepts Rust output as an input.
- [CPython v3.13.15 `unicodeobject.c`](https://github.com/python/cpython/blob/v3.13.15/Objects/unicodeobject.c):
  `do_title`, `lower_ucs4`, and `handle_capital_sigma`. Full mappings expand to
  at most three scalars. The generator observes Case_Ignorable with two direct
  contextual-sigma probes, including characters that are both cased and ignorable.
- [Unicode 15.1 UCD provenance](https://www.unicode.org/Public/15.1.0/ucd/ReadMe.txt).
  The inferred Case_Ignorable predicate also matched the official
  `DerivedCoreProperties.txt` for all 4,148 smoke scalars, including cased
  ignorables. That official file has 491 Case_Ignorable ranges; file SHA-256
  `f55d0db69123431a7317868725b1fcbf1eab6b265d756d1bd7f0f6d9f9ee108b`.
  This primary-data smoke comparison is not an exhaustive scalar run.
- [Exact Rich cells implementation](https://github.com/Textualize/rich/blob/v14.3.3/rich/cells.py).
  Installed code, loader and selected data-file SHA-256 values are recorded in
  the generated oracle. `auto` and `UNICODE_VERSION` are never used.

### Material version/policy differences

These are source observations, not guessed Rust expectations:

| Input | CPython UCD15.1 source policy | Rich14.3.3/UCD17 policy | Treatment |
|---|---|---|---|
| NUL | category Cc; scalar width 1; repr `\x00` | width 0 | Separate methods |
| `e` + U+0301 | source width 2 | width 1 | No wcwidth substitution |
| U+200D | category Cf, nonprintable; width 1 | scalar width 0; sequence skip rule | Preserve both policies |
| U+FE0F | category Mn, printable; source width 1 | scalar width 0; can widen prior glyph | Preserve sequence context |
| Woman + ZWJ + laptop | source width 5 | Rich width 2 | Scalar source wrapping remains |
| U+1FAE9 shovel | Cn, EAW N, width 1, nonprintable | width 2 | Full pinned source table, not a one-character exception |
| U+0378 / U+FFFF / U+10FFFF | valid scalars, Cn, EAW N, nonprintable | width 1 | Valid scalar queries, escaped repr |
| U+E000 private use | Co, EAW A, nonprintable | width 1 | Do not equate private use with printable |
| U+D800..U+DFFF | Python strings can contain these | not valid Rust scalars | Typed rejection; JSON contract unchanged |

ICU's UCD18 properties are exposed separately with their version. An exhaustive
native test will inventory UCD15.1-versus-18 category and F/W differences; its
actual totals are **pending** the CPU/build grant. The full generated source
database is the compatibility adapter for *all* source decisions, not a small
exception list and not silent adoption of newer ICU data.

## Implementation and exact integration requests

`hosts/cli/fabric-cli-output/src/production_unicode.rs` supplies:

- `ProductionUnicode::from_json(reviewed_embedded_data, UnicodeLimits)`:
  immutable, caller-injected data, strict version/shape checks, complete
  ordered scalar coverage including all noncharacters and unassigned scalars.
  It rejects overlaps, gaps, surrogate ranges, unknown property classes,
  unsorted mappings and unsupported version combinations.
- Existing `CharacterWidth`, `properties(char)`, `codepoint(u32)`,
  `icu_properties(char)`, `source_text_width`, `rich_text_width`.
- `title_key` and `python_string` with typed `UnicodeError`. Numbers still use
  `text::python_string` unchanged. No JSON encoder or serde value domain changes.
- `embedded_provider`, `width_provider`, `key_formatter` and `value_formatter`
  use a single immutable `LazyLock<Result<ProductionUnicode, UnicodeDataError>>`.
  The original initialization error, including its serde cause, remains
  accessible through `embedded_provider`. Callbacks classify failures as
  `TextError::UnicodeData(UnicodeDataKind)` or
  `TextError::UnicodeResource(UnicodeResourceKind)` without ASCII fallback.
  Their command error codes are `UnicodeData` and `UnicodeResourceLimit`.
  Original `UnicodeError::Text` values, including partial output, pass through
  unchanged. Existing JSON/text source error variants are not redefined.
- Hard caps: data input 4 MiB, 32,768 ranges, 8,192 mappings, 512 Rich widening
  scalars; operations at most 4 MiB input, 16 MiB output, depth 128 and 1,000,000
  nodes. Callers may only tighten these. Output is bounded while writing, not
  checked only after allocating an oversized result. Casing uses bounded linear
  lookahead, not repeated scans over long ignorable runs.

The controller approved immutable embedded data and these narrow typed text-error
variants. The module, callbacks and tests can be prepared while another physical
validation job runs; module export and default-callback selection must wait for
the complete independently verified corpus and the approved manifest edge.

Initial integration plan (library steps now completed; host steps remain separate):

1. The exact manifest/lock change above, then data generation and source review
   within a CPU slot. Do not export the module before its generated test files
   exist; staged tests deliberately `include_str!` those required artifacts.
2. Export this module from `lib.rs` and select the production key/value callbacks
   in `TextOptions::default` and the internal printer. Keep the explicit ASCII
   helper APIs and their source expectations unchanged.
3. Supply the immutable embedded provider to actual central host width consumers.
   The parent owns these host changes; library validation is not host completion.
   `TextOptions` currently requires **function pointers**, not captured provider
   closures. Its key/value callbacks also require `TextError`, whereas bounded
   Unicode operations return `UnicodeError`. A controller-owned wrapper needs
   a fixed, validated provider and typed/sanitized text resource error conversion.
   The prepared `key_formatter`/`value_formatter` callbacks now supply this
   composition and preserve `UnicodeError::Text` verbatim. Select those callbacks
   after corpus verification; central hosts can obtain the width provider
   through the typed `width_provider` accessor. No resource failure is reported
   as `InvalidKey`, `UnicodeRepr`, or `InvalidJson`, and no mutable per-command
   global configuration is introduced.
   The scalar-only `CharacterWidth` interface cannot enforce a whole-table
   input/output budget by itself. Admit/budget complete render inputs at the
   host/render boundary; do not claim the bounded whole-text methods impose
   those limits on unrelated existing renderer callers.
4. Update the standalone output README and compatibility ledger to document the
   validated library API, policies and limits, without claiming native host
   integration. Keep provenance and validation history in this contract, not
   in README files.
5. Have independent reviewers check the actual combined revision and native
   tests, not just this unexported module.

There is no production Python requirement: the generator is development-only;
the host embeds the reviewed data. Loading JSON is pure and can happen once at
caller initialization. Structurally valid data alone does not establish trusted
provenance: retain independent `--verify`, artifact hashes, licensing and review.

## Additive independent baseline and finite validation plan

The generator pins **CPython 3.13.15/UCD15.1.0 and Rich14.3.3/UCD17.0.0**.
It refuses version drift, requires `--full-scalar-slot` for full generation or
verification, and refuses to overwrite any existing artifact when generating.
Each full sweep has a 45-second deadline, checked every 4,096 scalars and again
after whole-text capture; each JSON artifact is capped at 4 MiB.
It creates only new `source-data.json`, `oracle.json`, `PYTHON-LICENSE.txt`.
Existing fixture files, hashes and source expectations are never changed.

Full generation covers exactly **1,112,064 valid scalars** as contiguous verified
ranges, excludes all 2,048 surrogate code points, captures category/EAW/predicate/
Rich-width histograms and a scalar-stream FNV-1a checksum, plus canonical range
and data SHA-256. It also records all nonidentity full lower/title mappings,
actual-source key cases at every mapped scalar, 48 adversarial whole-text cases,
31 additional keys, 62 JSON-domain str/repr cases, and 40 actual-source tables.
NUL, controls, noncharacters, private use and unassigned scalars are in the full
domain, not silently excluded. `--verify` independently re-executes Python/source
for the entire data set; Rust observations are never read.

The native test module checks every scalar's measured properties and checksum,
all generated casing cases, real renderer consumers and whole-text sequences,
numeric/JSON stability, invalid integer code points, malformed databases, and
finite resource failures. Source wrapping captures are additionally retained
in the oracle; actual source-versus-Rust table tests exercise the existing
wrapping consumer, without replacing that algorithm.

Historical first-stage checks (Windows, 2026-10-29):

| Check | Actual result |
|---|---|
| Independent generator guard unit tests | 10 passed, 2.573 seconds; includes exclusive writes, cleanup and exact-byte verification |
| Existing unchanged actual-source text/table oracle | 83 cases passed |
| New bounded property smoke | 4,148 scalars, 48 whole texts, 31 keys, 62 values, 40 source tables captured |
| Official UCD15.1 Case_Ignorable comparison | All 4,148 sampled scalars matched |
| Black AST-safe format/check | Passed with explicit `--target-version py313` |
| Pinned 1.99.1 standalone rustfmt/check | Passed for new provider and native test module |
| Generated complete artifacts / exhaustive Rust tests | Not run; CPU slot and controller wiring required |
| Cargo resolution/build/test/Clippy | Not run; no Cargo slot or manifest authority granted |

These are author-side small checks, not independent review or native compilation.
While working, the root manifests became dirty concurrently; this lane did not
edit or restore them. The output manifest, exports, existing output modules and
old fixtures remain untouched by this lane.

At that first stage, the full sweep and Cargo selectors below were unrun.
Their subsequently completed results are recorded in the final section:

```powershell
# From rust\, only after the controller grants the full-scalar CPU slot:
python -B hosts\cli\fabric-cli-output\tests\production_unicode\generate.py --generate --full-scalar-slot
python -B hosts\cli\fabric-cli-output\tests\production_unicode\generate.py --verify --full-scalar-slot

# After approved dependency resolution and module export, within the serial build slot:
cargo test --offline --locked -p fabric-cli-output production_unicode -- --test-threads=1 --nocapture
cargo test --offline --locked -p fabric-cli-output -- --test-threads=1
cargo clippy --offline --locked -p fabric-cli-output --all-targets -- -D warnings
```

Use the existing metadata-derived architecture/downstream/native validation plan
for integration; these package commands do not replace it.

The bounded smoke measured **4,148 scalars in 0.0177–0.0265 seconds**, projecting
only the scalar-record/checksum portion to approximately **4.74–7.12 seconds**. This is
not a measured full-generation runtime: histogram, mappings, source casing,
artifact writing and verification add work. Request **one serial CPU slot,
120 seconds maximum for generation plus independent verification**, expected
artifacts below 4 MiB each and working memory below 128 MiB. Stop/report rather
than extending that budget silently. Native debug exhaustive tests and first
ICU compilation need a separate controller-granted Cargo slot; allow up to
180 seconds for the focused output test command before reporting a blocker.

The first-stage gates were manifest application, the full-scalar slot, generated
artifacts/license capture, native compilation and callback composition. These
library gates are now satisfied. Independent review, downstream/native host
validation and cross-platform/production acceptance remain outstanding.

## Historical phase-two preparation boundary

The immutable embedded-provider accessors, typed initialization/operation
failures, default-callback implementations and additional native tests are
implemented in the owned output source. The native tests also require default
selection and compare three additional real-source Unicode renderer/partial
output cases. They are intentionally not registered by exporting the module
before its required generated files exist.

`TextError` now has the approved typed data/resource variants; legacy ASCII
helper behavior, old fixtures, old source hashes and JSON encoding are unchanged.
The first-stage README's provenance, primary-source references and handoff plan
were moved into this contract. The replacement test README contains only
purpose, API, supported limits, testing and licensing information.

During preparation, full generation/verification, Cargo, module export and default
selection remained blocked on the subsequent explicit grant and controller
manifest change. The announced job-end time was not treated as permission.
Generation and independent verification subsequently preceded export and serial
native validation; no mutable per-command configuration was introduced.

Phase-two bounded checks passed: standalone pinned rustfmt/check for the provider,
typed text errors and native test module; Black's AST-safe Python check; and all
10 generator guard tests (1.942 seconds). The three added source-render captures
retain the existing 62-value cohort. One newly introduced generator variable
shadowing defect was caught by that unchanged count assertion and repaired before
any corpus generation. At that preparation checkpoint, full-scalar and native
tests had not yet run.

## Completed serial validation

Grant `controller-icu-production-provider-20261029` ran on Windows
`x86_64-pc-windows-msvc`, rustc 1.99.1
(`58cf8f9c35b355199727627d07ae34a4e2846d67`). The same original
`Local\FabricRustParityValidation` mutex handle was held from nonblocking
acquisition through corpus generation, validation, one repair and final cleanup.
A separate process proved exclusion while held. CPU affinity selected two CPUs,
Cargo jobs were two and tests used one thread.

The whole lease lasted **259.669 seconds**, within the single 900-second deadline.
Full generation and independent verification took **23.280 seconds aggregate**
(11.785 and 11.494), within 120 seconds. All 17 owned command processes exited;
no direct descendants remained in the post-check, and a fresh nonblocking mutex
probe succeeded after release. The receipt contains ordered string-keyed records,
exact commands, PIDs, exits, timing, cleanup and unchanged manifest hashes.

The existing isolated oracle environment initially lacked Rich. Only after that
actual failure, `rich==14.3.3` was installed with `--no-deps` using this oracle's
development requirements. No global installation occurred. The runtime asserted
CPython 3.13.15, `unicodedata` 15.1.0 and Rich 14.3.3; every Rich call explicitly
selected Unicode 17.0.0, never `auto` or an environment default.

### Complete generated corpus

| Evidence | Actual result |
|---|---|
| Valid scalars generated, independently verified and checked natively | 1,112,064 |
| Inclusive complete property ranges | 4,458 |
| Nonidentity full casing mappings | 2,881 |
| Source key cases over all mappings | 8,643 |
| Rich narrow-to-wide scalars | 213 |
| Whole-text / extra keys / str-repr values | 48 / 31 / 62 |
| Actual-source tables / default renderer cases | 40 / 3 |
| Unassigned/noncharacter category Cn coverage | 824,718 |
| Private-use category Co coverage | 137,468 |
| Printable / alphanumeric / whitespace scalars | 149,625 / 138,557 / 29 |
| Rich zero / one / two-cell scalar classes | 6,782 / 922,423 / 182,859 |
| Canonical runtime-data bytes | 177,045 |
| Scalar-stream FNV-1a | `2c09ded6c3412fca` |
| Canonical range SHA-256 | `59861f9735aed139d746216d86943868eac9c79e7b7cc128a75b78f7e92abb0c` |
| Canonical runtime-data SHA-256 | `a5108a268789290c7fcbda71a5edcc66e1f8964e6f7e165e227250b31383bdf8` |

Coverage includes all 66 noncharacters by complete-domain range coverage, all
unassigned scalars and all private-use scalars, without accepting any of the
2,048 surrogate code points. The runtime loader rejects gaps even in these
particular slices; native negative tests exercise their removal.

The native complete-domain comparison observed **22,997 general-category** and
**1,345 F/W-width** differences between source UCD15.1 and ICU UCD18. These are
reported version differences, not modified source expectations. The complete
source adapter preserves the former for source formatting; ICU properties and
Rich sequence measurement remain separate APIs.

Exact installed upstream source SHA-256:

- Rich cells: `7cf4529eab989423b84c782652890e24e84d95cc7c0b8359d97bfd24cffb19b1`
- Rich data loader: `e343e1005a776fcf323e746d4c6ea2f60e718fee70f7c0844f70b8229d4b2402`
- Rich Unicode 17 data: `3576e316c01a4d8763e9362093943d762b2fe342d91db79d146465ff8f3ff58d`

The oracle records all seven original CLI Git blob hashes unchanged, plus
the full Python license and license-source hashes. In particular, the real UI
source remains `1ff741b5838ca0c7d8c003fd60233804bd6df15f` at the pinned CLI source
commit. No old source pin, oracle expectation or hash was repinned.

### Commands and results

All commands below exited **0** on the repaired final source:

```powershell
python.exe -B hosts\cli\fabric-cli-output\tests\production_unicode\generate.py --generate --full-scalar-slot
python.exe -B hosts\cli\fabric-cli-output\tests\production_unicode\generate.py --verify --full-scalar-slot
python.exe -B -m unittest discover -s hosts\cli\fabric-cli-output\tests\production_unicode -p test_generate.py
python.exe -B tests\compatibility\output\text_reference.py
python.exe -B -m unittest discover -s tests\compatibility\output
cargo test --offline --locked -p fabric-cli-output --all-targets -- --test-threads=1 --nocapture
cargo test --offline --locked -p fabric-cli-output --doc -- --test-threads=1
cargo check --offline --locked -p fabric-cli-output --all-targets
cargo clippy --offline --locked -p fabric-cli-output --all-targets -- -D warnings
cargo tree --offline --locked -p fabric-cli-output -e features
```

The runner's receipt records exact working directories and the absolute isolated
Python executable. Results: **29 Rust tests**, including 14 Unicode tests;
**one doctest**; **10 generator guard tests**; **16 retained oracle guard tests**;
and the unchanged **83 real-source text/table cases**. Check, strict all-target
Clippy and the feature graph passed. The graph selects only the already-resolved
ICU properties 2.4.0 compiled-data edge; manifests, locks and registry blocks were
unchanged throughout this lane's validation.

The first native run exited 101 and exposed two issues. Source `rstrip` also
strips Python's four C0 whitespace separators, unlike Rust's `trim_end`; table
padding and source truncation now consult the **complete generated whitespace
property**, not a hand-maintained special-case list. A newly authored budget test
also enabled a stricter node limit before its intended input-byte failure; the
test now isolates the input budget. Neither fix changed any generated source
artifact or old expectation. One repair batch was followed by the full successful
test/doc/check/Clippy sequence; the three-cycle budget was not exhausted.

Logs and structured receipt are retained under:

```text
rust\target\unicode-provider-validation\controller-icu-production-provider-20261029\
```

`receipt.json` records every exit and cleanup result; `evidence.json` binds the
tested source/data hashes and post-release cleanup proof. The first-failure logs
are retained alongside final `*-cycle1` logs, rather than overwritten.

### Remaining integration and acceptance

The exported integration API is
`production_unicode::{width_provider, embedded_provider, key_formatter, value_formatter}`.
`TextOptions::default` and the internal printer already select the key/value
callbacks. The parent must still wire `width_provider()` into actual native
central consumers and preserve its typed failure, without silently selecting
`ascii_width`. JSON rendering continues through its existing bypass.

The source/runtime bounds remain 4 MiB input, 16 MiB output, depth 128 and
1,000,000 nodes, with explicit typed failures. Whole-table admission remains the
render caller's responsibility. Arbitrary terminal-toolkit markup/backend
behavior, live hosts, cross-platform execution, independent physical review and
production-GA acceptance are not established by these Windows library checks.
