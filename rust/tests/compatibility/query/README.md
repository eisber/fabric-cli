<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Query projection contracts

This suite verifies JMESPath evaluation, property-path discovery, scalar
presentation and output composition against fixed synthetic expectations.
Object ordering and integer-versus-float distinctions are part of comparison.

The reference requires the pinned development JMESPath package and verifies
input identities before guarded execution. It does not initialize the
application, discover credentials, send requests or regenerate fixtures.

From `rust/`, using a caller-owned UTF-8 observation path:

```powershell
cargo run --locked --offline --quiet -p fabric-cli-query --example projection_output |
    Set-Content -Encoding utf8NoBOM $observations
python -B tests\compatibility\query\reference.py --observations $observations
python -B -m unittest discover -s tests\compatibility\query -p test_reference.py
```

Obtain the applicable execution grant before building on a shared host.
The example composes actual query results, the output envelope and supplied
writers; it does not accept expected-output callbacks.

Unsupported domains retain explicit error expectations. Negative controls cover
input mutation, omitted cases, changed trees/bytes and forbidden I/O.
Separate call-site tests preserve the different empty-query policies of LS,
GET and raw API commands.
