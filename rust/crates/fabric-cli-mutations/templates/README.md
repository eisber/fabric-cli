<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Pinned native creation templates

These definition data files are byte-for-byte copies from
`microsoft/fabric-cli` commit `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`src/fabric_cli/commands/fs/payloads`. Existing data has not been reformatted,
given new headers, regenerated or read from a Python installation at runtime.
The native creation module embeds the required bytes with `include_bytes!`.

`tests/creation_reference.py` verifies each copied file against the exact git
object, then executes the original source helper using a separate pinned-source
export. Raw definitions and any encoded/modified JSON are compared at the public
request consumer boundary, not regenerated from Rust expected outputs.
The existing `MutationHost::create` preserves the historical bundled fixture
order (observed on Windows), not a natural-host-order guarantee. Python's
unsorted `os.walk` order is an ambient input, not a portable fixed sequence.

`create_with_template_enumeration` accepts caller-owned `TemplateEnumeration`:
template names map to top-down `TemplateDirectory` rows with relative directory,
ordered immediate child directories, and ordered files. The empty directory is
the root; relative directory separators are `/` on every platform. All consumed
templates must be supplied, including SemanticModel dependencies of Reports.
The native embedded consumer preserves both supplied directory traversal and
file order, without changing bytes, codecs or Report dataset-reference rewrites.
The library does not read a filesystem, invoke Python, or select an OS-specific
sequence. A future ambient listing adapter belongs to the caller.

Each consumed trace must enumerate the complete pinned tree exactly once in the
declared top-down directory order. Missing files/directories/templates, duplicate
entries, invalid paths and inconsistent traversal return
`InvalidTemplateEnumeration`. This is an explicit bounded input safety
classification: synthetic Python walk collaborators can emit duplicate files;
the native contract rejects these rather than silently deduplicating them.
Unused template entries are not consumed or validated.

The development-only source harness binds independently authored listing inputs
to the unchanged pinned `_create_payload`'s `os.walk` collaborator and serializes
the same inputs to the real native command/SDK consumer. The existing fifteen
comparisons remain order- and byte-sensitive. Additional permutations cover
root/nested file ordering, Report directory traversal, Report rewrites and
dependent creation, and default/dynamic single-part mirrors. Independently
authored expected path sequences and pinned raw bytes are checked, not sorted
or regenerated from native results. This proves supplied-input parity only:
ambient/uninjected filesystem-order equivalence remains unproven, and the
default embedded sequence must not be presented as natural-host equivalence.
