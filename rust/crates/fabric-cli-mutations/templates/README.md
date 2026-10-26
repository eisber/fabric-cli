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
Parts preserve the observed original source traversal order; Python's unsorted
`os.walk` ordering across other native filesystems remains an explicit evidence
boundary, not a claim that a sorted-definition redesign was approved.
