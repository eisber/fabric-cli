Copyright (c) Microsoft Corporation. Licensed under the MIT License.

fab-rs — UNSIGNED, UNOFFICIAL PREVIEW

This bundle is not an official Microsoft release, installer, updater, signature,
or security/compliance certification. Do not bypass OS trust policies.

Verify SHA256SUMS against a trusted artifact source before extracting. Checksums
detect byte changes; they do not prove publisher identity. Keep the adjacent
target-qualified SBOM and provenance JSON files with the download.

Extract the ZIP (Windows) or tar.gz (Linux/macOS) into a directory you control.
Run .\fab-rs.exe --help on Windows or ./fab-rs --help on Linux/macOS.
Run --version to confirm the version. Optionally add that directory to PATH.
This does not replace the Python fab command. No Python runtime is needed.
OS native libraries and certificate trust stores remain runtime prerequisites.
Consult provenance for the observed build host and native loader requirements;
minimum supported operating-system versions have not yet been established.

For authentication, install Azure CLI separately and sign in yourself using
az login. Then run fab-rs auth login to bind the current Azure CLI identity.
fab-rs never signs you into or out of Azure CLI. Its auth logout clears only the
Rust binding. No live authentication is needed for --help or --version.

Only auth login/status/logout and raw api are supported. API requires a Rust
login. Recognized Fabric/ARM long-running operations poll by default, with a
fixed 600-second budget and 1-second doubling fallback. --async returns the
initial response without polling or starting background work. Query parameters
belong in the quoted endpoint; --query is local JMESPath filtering.
There are no --params, --wait, --no-wait, or polling tuning flags.

LICENSE contains the product MIT license. THIRD-PARTY-NOTICES.txt contains
dependency license/notice texts for the conservative full resolved Cargo graph,
including build/dev dependencies. This is not a claim all are linked.
