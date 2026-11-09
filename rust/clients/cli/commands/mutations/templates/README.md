<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Item creation templates

These definition files are embedded with `include_bytes!` and supplied to item
creation. Preserve their bytes, file names and directory structure; they are
request data, not generated documentation.

## Enumeration

`MutationHost::create` uses the bundled enumeration order.
`create_with_template_enumeration` accepts a caller-owned `TemplateEnumeration`
when ordering must be explicit.

Each template contains top-down `TemplateDirectory` rows with a relative
directory, ordered immediate child directories and ordered files. The empty
directory denotes the root. Relative separators are `/` on every platform.
Include all consumed templates, including SemanticModel dependencies for Reports.

The consumer preserves traversal and file order, raw bytes, encoding and Report
dataset-reference replacement. It performs no ambient filesystem enumeration.

## Validation

A consumed enumeration must cover its complete tree exactly once. Missing
templates, files or directories, duplicate entries, unsafe paths and inconsistent
traversal produce `InvalidTemplateEnumeration`. Unused entries are not consumed.

Tests check fixed raw bytes, request payloads, ordering permutations, Report
rewrites and dependent creation. Default bundled order is deterministic; it is
not a guarantee about an operating system's filesystem enumeration order.
