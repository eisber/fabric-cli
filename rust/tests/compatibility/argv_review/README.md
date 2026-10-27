<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded argv source capture

`capture.py` executes the original parser classes, registrations, validators,
error classification, help formatter and lazy dispatch identities from Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. Its 32 representative cases cover
jobs, local/configuration commands, reads, resource/access and definition-transfer
arguments. This is not a full command inventory or CLI parity claim.

The source is read from immutable Git objects, never installed product imports.
An optional `--source-archive` accepts those same Git blobs with an exact pin,
base64 content and SHA-256 digests, for a Python interpreter that cannot access
a Windows Git worktree. The producing caller must verify the archive against
the pinned Git objects; a digest alone is not independent source provenance.
No expected observations are derived from Rust.

The logger delegates to the original Python warning function. Only its final
rich-terminal boundary is injected as plain text with original stream selection;
group-help rendering is captured as an event. Help for individual leaves uses
the original help formatter. These observations are not byte-exact full terminal
or root-process output. Accepted command handlers are identified but never
executed. No configuration, authentication, browser or service call occurs.

From `rust`, write an additive evidence capture to an absolute, new destination:

```text
python -B tests/compatibility/argv_review/capture.py --output <new-absolute-json-path>
```

Add `--rust-binary <exact-build-fab-rs>` only when that binary's source and features
are independently verified. This optional mode clears inherited identity and
configuration inputs and records subprocess streams/exits; it does not build
anything or assert successful remote execution. Without it, Rust execution is
explicitly marked unrun. Output captures refuse overwrite.

The initial source review uses Rust
`03fab20577a1016178de227eac0ee178921bb8d0` as its reference, not as evidence
that a current binary was executed. Production repairs and actual default/native
Rust test results require their own immutable candidate and authorized runner.
