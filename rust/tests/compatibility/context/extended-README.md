<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Extended context contracts

`extended_cases.json` covers virtual resources, OneLake traversal and explicit
local-path fallback. It supplements the remote context suite without replacing
its fixtures.

Cases supply current/home context, ordered catalogs, warnings, notices,
properties and local metadata. Windows and POSIX path modes are explicit test
inputs, not inferred from the runner.

## Assertions

Every result includes path, ID path, kind, name and ID. Subtypes describe
workspace, virtual-container, item and OneLake kinds. Missing resources retain
null IDs rather than appearing resolved.

Ordered lookup, properties and local-operation scripts must be consumed
exactly. Duplicate requests require repeated script entries. Success and failure
must preserve current-object identity, catalog contents and parent links.
Header values retain case-sensitive resource and shortcut semantics.

The reference validates recorded inputs before execution. Lookup responses,
home and local metadata are injected; no actual file descriptors, credential
stores, network requests or ambient filesystem probes are used.

From `rust/`:

```text
python -B tests\compatibility\context\extended_reference.py
```

Run through the applicable shared-host gate. Negative controls deny imports,
files, environment discovery, authentication, process creation and sleeping.
These synthetic boundaries do not establish native permission behavior,
persistence, cache invalidation or live OneLake authorization.
