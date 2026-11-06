<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Semantic-model BPA source acceptance fixtures

Independent source: `microsoft/semantic-link-labs` commit
`2ddf46109c1209ceffa88149f4631e7b2275f9a1`. See the developer runner and full
coverage limits in `rust\tests\compatibility\semantic_model_bpa\README.md`.

| Path | Purpose |
| --- | --- |
| `inventory.json` | All 64 rules: exact metadata, scopes, expression/source-line/AST provenance, dependencies and result columns |
| `predicate_cases.json` | 145 immutable synthetic inputs, original-expression outcomes and observed collaborator calls; 62 rule expressions exercised |
| `evaluation_cases.json` | Seven bounded original-evaluator cases: empty model, clean dataframe, violation, predicate error, malformed JSON, relationships/model scope, and multi-scope names |
| `coverage.json` | Per-rule coverage, unverified helper semantics, two unexecuted external-provider rules and all runtime gaps |
| `capture_manifest.json` | Exact-byte hashes of every captured artifact and source/model input |
| `models\empty.bim` | Synthetic model with no tables |
| `models\decimal.bim` | Clean numeric data type for selected rule 1 |
| `models\double.bim` | Numeric metadata violation for selected rule 1 |
| `models\missing-source-column.bim` | Deliberate null data causing original rule 27 to raise |
| `models\malformed.bim` | Deliberately malformed JSON, not a purported TOM deserialization result |
| `models\relationships.bim` | Explicit many-to-many/bidirectional relationship and differing column types |
| `models\naming.bim` | Case, whitespace and control-character metadata across table/column scopes |
| `upstream\_model_bpa_rules.py` | Exact public pinned rule/helper source bytes |
| `upstream\_model_bpa.py` | Exact public pinned evaluator source bytes |
| `upstream\_helper_functions.py` | Exact public source for original result-name helpers; module body never runs |
| `upstream\LICENSE` | Original upstream MIT license |

JSON expectations were captured from the **original verified AST**, not from
managed/Rust port output or handwritten predicates. The source files are inert
fixtures, never imported as modules. Tests supply synthetic TOM objects and
explicit helper responses only. Actual sempy, pandas, TOM, Fabric, XMLA,
authentication, notebook, Spark and product startup are not used.

The normalized result representation uses textual severity and `null` for
missing tuple metadata, not notebook icons or pandas missing-value encodings.
Empty model, empty dataframe, absent evidence, and data errors remain distinct.
The full default renderer and real TOM BIM/TMDL loading remain unexecuted.

Do not overwrite or regenerate these fixtures to match implementation changes.
The capture switch refuses existing baselines. Missing source bytes or a source
hash mismatch fail verification rather than fetching or accepting a moving pin.
