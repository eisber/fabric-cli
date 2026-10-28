<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Context resolution contracts

Fixed synthetic catalogs verify absolute, relative, home, workspace, folder
and item path resolution. Ordered lookup calls, parent relationships, exact
names, missing resources and first-match selection are observable contracts.

The reference uses injected current/home context and metadata lookups.
It does not discover identity, touch the filesystem, mutate environment state
or perform network requests. Fixture and recorded-input identities are checked
before execution.

Every success or failure must preserve the input catalog and current context.
Explicit context-changing APIs have separate atomicity tests. Unsupported
branches retain their classification and are not counted as successful cases.

From the repository root:

```powershell
python -B rust\tests\compatibility\context\reference.py
```

Run through the authorized validation gate on shared hosts. Negative controls
check source drift, altered expectations and forbidden I/O. Fixed expectations
must not be regenerated from native outputs.
