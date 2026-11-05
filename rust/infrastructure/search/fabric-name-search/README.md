<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Pure name suggestions

`fabric-name-search` is a dependency-free, frontend-neutral SDK-layer leaf.
It ranks **names supplied by its caller**. It does not discover candidates,
read values, perform I/O, authenticate, create a runtime, call a CLI, or mutate
anything. It is reusable for variable identifiers, resource names, local
configuration keys, or other caller-owned identifier collections.

Suggestions are advisory only: never autocorrect, select a different target,
authorize an operation, or retry a mutation using a suggested name. In
particular, this library introduces no Variable Library variable-delete API.

## API

```rust
use fabric_name_search::{SearchOptions, suggest_names};

let candidates = ["production_endpoint", "archive_bucket"];
let hints = suggest_names(
    "production_endpont",
    candidates,
    SearchOptions::default(),
).expect("bounded synthetic input");
assert_eq!(hints[0].name, "production_endpoint");
// Present an escaped advisory hint; preserve the original failed operation.
```

The public function is:

```text
suggest_names<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    options: SearchOptions,
) -> Result<Vec<NameSuggestion<'a>>, SearchError>
```

`NameSuggestion` contains the borrowed original `name: &'a str` and a finite
`score: f64` in `[0, 1]`. It does not return normalized or synthesized names.
Byte-exact query matches are excluded, byte-exact duplicates are collapsed, and
results sort by descending score then ascending original name (Rust string
ordering). Enumeration order never affects results.

For another use, a caller can pass its existing configuration schema names:

```rust
use fabric_name_search::{SearchOptions, suggest_names};

let keys = ["requestTimeout", "retryLimit", "outputFormat"];
let hints = suggest_names("requestTimeuot", keys, SearchOptions::default())
    .expect("bounded synthetic input");
assert_eq!(hints[0].name, "requestTimeout");
```

`SearchOptions::default()` returns at most five names with scores of at least
`0.6`. `max_results` must be `1..=20`; `min_score` must be finite and in
`0..=1`, inclusive. NaN, infinities, and invalid ranges return typed errors.
Threshold comparison is inclusive. Zero lowers the score filter but does not
disable conservative edit eligibility; one admits normalized case equivalents
only, not byte-exact matches.

## Scoring choice and limits

This is identifier typo ranking, not document retrieval. BM25 is useful for
corpus term-frequency relevance, but does not directly model an omitted letter
or adjacent transposition in a short identifier. This implementation instead
uses deterministic **optimal string alignment (OSA)**, the restricted
Damerau-Levenshtein distance: insertion, deletion, substitution, and adjacent
transposition each cost one. OSA does not repeatedly edit the same substring;
it is not unrestricted Damerau-Levenshtein distance (`CA` to `ABC` costs three,
not two). No corpus statistics or external dependency is needed.

1. Lowercase Unicode scalars using Rust's locale-independent lowercase mapping.
   The mapping can expand a scalar. No Unicode canonical normalization, full
   case folding, grapheme segmentation, accent stripping, or transliteration is
   performed. Composed and decomposed input remain distinct but safe.
2. Case-equivalent names score `1`. Otherwise, a name with only one original
   scalar has no non-case typo alternatives, even at threshold zero.
3. Consider only names within `max(1, floor(longest_lowercase_length / 3))`
   full-name OSA edits. The full-name score is `1 - distance / longest_length`.
4. Split lexical tokens at snake/kebab, whitespace/control, `.`, `/`, `\`, `:`,
   camel-case and acronym boundaries. Combining marks remain in their token.
   When concatenated tokens match, the lexical score is one. When token counts
   match, the lexical score is the weakest corresponding token's normalized
   OSA score (or the full score for a single token). Otherwise it is zero.
5. The final score is the lesser of the full-name and lexical scores. Zero
   scores are omitted, then the requested threshold and result limit apply.

The weakest-token cap prevents a long common prefix or suffix from drowning
out an unrelated distinguishing token. For example, `production_endpont` and
`prodcution_endpoint` are close to `production_endpoint`, whereas
`production_account` is not boosted solely by the `production` token.
Scores are heuristic similarities, not confidence probabilities. Conservative
false negatives are intentional: token-boundary-changing typos can be omitted
unless the concatenated tokens are identical; very short names, large edit
distances, token reordering, and canonically equivalent Unicode spellings may
also yield no hints. Callers must never treat absence of hints as absence of a
valid name.

## Input and computation budgets

Public limits apply before filtering, exact-match exclusion, or deduplication:

| Limit | Inclusive budget |
| --- | --- |
| `MAX_NAME_CHARS` | 512 Unicode scalars per query or candidate |
| `MAX_CANDIDATES` | 4096 enumerated candidates, including duplicates |
| `MAX_TOTAL_BYTES` | 1,048,576 cumulative UTF-8 bytes, including the query |

Options are checked first. All bounded input is validated before scoring,
including empty/whitespace queries and candidates appearing after a full result
set could have been found. Cap-plus-one input returns a typed `SearchError`,
never a truncated or prefix-result success. Enumeration stops at the first
violation; multiple simultaneous violations have no promised error precedence.
Empty/whitespace queries otherwise return no suggestions.

OSA retains only three rows, never a full distance matrix. The full-name pass
uses a diagonal edit-budget band; token passes are bounded by token lengths.
Scratch storage is linear in normalized name length, and worst-case distance
work is quadratic per distinct bounded name, not unbounded input. Row clearing
also has quadratic worst-case work. Stored original names are only borrowed
references in a bounded deduplication set. The caller owns candidate collection
and iterator behavior; this synchronous API does not cancel or sandbox a
caller-supplied iterator.

## Untrusted data and diagnostics

Queries and candidate names are untrusted, including control characters.
`SearchError` uses payload-free typed variants; `Display` and `Debug` never
echo input fragments. `NameSuggestion::Debug` redacts names. Direct access to
`name` deliberately returns original bytes, so presentation adapters must
escape controls and apply their own disclosure policy. This crate has no
logging, value fields, hidden discovery, transport, or authentication path.

Tests contain only synthetic names and cover meaningful edit distances, OSA's
restricted behavior, band boundaries, lexical noise, Unicode, deterministic
ordering, thresholds, redaction, and inclusive/cap-plus-one input budgets.
