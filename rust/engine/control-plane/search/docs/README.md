<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline Fabric documentation search

`fabric-docs-search` reads an immutable, version-pinned Tantivy documentation
snapshot. Consumers own the reader and can reuse it across queries. The library
doesn't discover credentials, contact services, extract files, create a writable
cache, or initialize a runtime.

The optional `index-builder` feature enables the separate offline producer.
Ordinary readers use static embedded byte slices, not the builder's filesystem
mapping. `DocsSearch::copied_embedded` is an explicit measurement control, not a
fallback. Missing or incompatible indexes fail.

Build the executable with `FAB_DOCS_INDEX` set to the absolute directory
containing a finalized `bundle.json` and its checksummed component files.
Without that variable the package builds an **engine-only** configuration:
`DocsSearch::embedded` returns `SearchError::NotEmbedded`. It never substitutes
sample documentation or downloads data.

`DocsSearch::search_rich` adds lexical stemming, reviewed query-time acronym
expansion, bounded prefix matching, source-backed filters/facets, finite-candidate
reranking, article grouping, ancestor-heading context, and UTF-8 snippet highlight
spans. `DocsSearch::search` retains the narrower section-level contract. Completion
returns only titles/headings from the indexed corpus; no model is invoked.

The v2 index schema declares its populated capabilities in `manifest.features`.
The builder's `--profile baseline|stemmed|metadata|hierarchy|autocomplete|full`
supports controlled size experiments; `full` is the production feature profile.
An ablation profile is not a full-feature distribution, and requesting a missing
index capability fails explicitly. Metadata filters use observed exact source
keys/values, not inferred taxonomies. The version-pinned `data/item-types.json`
adds transparent exact-article classifications only; unmapped articles remain
unclassified.

`tests/performance/docs_space_impact.py` verifies actual matched bundle components
and compiled executable captures. Independent additions are compared with one
baseline; their deltas are not assumed additive. Corpus, codec, toolchain,
source/lock identity, and executable flags must remain fixed.

See [documentation search](../../../../authoring-docs/docs-search.md) for corpus
preparation, attribution, query bounds, CLI usage, and measurement methodology.
