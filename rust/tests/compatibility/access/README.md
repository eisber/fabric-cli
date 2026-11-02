<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Access command contracts

This suite checks ACL listing and mutation, local labels, projection, text/JSON
presentation and export intents with synthetic inputs and fixed expectations.

Fixtures cover resource routes, role validation, principal probing, confirmation,
assignment identity, ordered request bytes, stream placement and error
normalization. Operation cases distinguish acceptance from completed work.
Branch ledgers are inventories, not coverage or command-completion percentages.

Reference runners verify recorded input identities and execute with injected
HTTP, UI, time and file boundaries. They do not authenticate or contact services.
Normal execution compares immutable fixtures and never rewrites expectations.

From `rust/`:

```text
python -B tests\compatibility\access\reference.py
cargo test --offline --locked -p fabric-cli-resources --test access_contract
```

Additional runners check labels, mutations, wire encoding and rendering.
Native consumers exercise the actual SDK, pager, shared HTTP, projection and
output stack. Separate negative tests enforce origins, bounds, cancellation,
partial failures and output-delivery errors.

These are offline contracts, not live authorization or terminal-platform
acceptance. Use the applicable resource grant on shared hosts.
