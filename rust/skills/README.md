<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Rust CLI workflow skills

This directory reserves the documentation boundary for future Rust CLI workflows.
No service or deployment skills are implemented in this scaffold. The existing
Python CLI skills elsewhere in the repository remain unchanged; Rust workflows
must not depend on their scripts at product runtime.

Skills compose CLI invocations and interpret documented JSON/exit-code contracts.
They must not duplicate SDK service logic, acquire credentials independently,
embed secrets, or depend on an agent runtime being installed with the CLI.
Direct SDK consumers remain supported independently of skills and the CLI.

Today `fab-rs` implements help and version only. Do not describe authentication,
service operations, or deployment as available. New command namespaces and
workflow skills require separate design and compatibility review.
