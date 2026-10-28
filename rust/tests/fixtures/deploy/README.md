<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Deployment contract fixtures

These JSON fixtures define deterministic deployment input and output contracts.
All data is synthetic and Microsoft-authored under the repository MIT license.
Names, identifiers, paths and workspaces are invented. The fixtures contain no
credentials, tenant snapshots or recorded service responses.

## Fixture files

| File | Contracts |
| --- | --- |
| `yaml.json` | Scalar typing, duplicate-key handling and YAML traversal |
| `config.json` | Environment-specific settings and configuration extraction |
| `ids.json` | Dynamic references, logical identifiers and workspace substitution |
| `transforms.json` | Key/value replacement, text matching and item filters |

Each case has a unique `id`, an `operation`, inputs, and either an exact
`expected` value or an error classification with a diagnostic substring.
Optional `calls` fields specify the order of injected metadata callbacks.
Fixture documents retain machine-readable provenance.

Strings are compared exactly, including whitespace, escaping, key order and line
endings. Other JSON values preserve their types: `true` is not interchangeable
with `1`. Scalar type records keep dates and nonfinite values explicit rather
than silently coercing them into the JSON domain.

These fixtures cover individual contracts. They do not replace command-level
tests for metadata requests, filesystem effects, cancellation, publication or
partial failures.

## Reference checks

`reference_oracle.py` checks the fixed expectations against an explicitly
supplied local reference checkout. It verifies the recorded revision and selected
file hashes before executing extracted definitions. It does not initialize a
service client, discover credentials, perform network requests or rewrite
expectations.

The checker requires an isolated development environment with an interpreter
version of 3.11 or later, PyYAML 6.0.3 and jsonpath-ng 1.9.1. Dependencies are
development-only and must not be installed globally on a shared host.

From the repository root:

```powershell
python rust\tests\fixtures\deploy\reference_oracle.py --reference-root C:\path\to\reference-checkout
```

Execution on a shared host requires the applicable resource grant. A successful
report records the actual platform, dependency versions, case count and content
hashes. Retain reports as validation artifacts, not replacement baselines.

Injected callbacks use fixed metadata and perform no I/O. Filesystem discovery,
real service metadata, credential behavior and cross-workspace resolution require
separate integration tests.
