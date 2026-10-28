<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Table contracts

This suite covers table load options, maintenance configuration, Delta schema
selection, resource eligibility, retention, request bytes and presentation.
All inputs and expected results are synthetic and fixed.

Lifecycle fixtures distinguish bare acceptance, operation-ID polling, result
retrieval, failure, cancellation and unknown states. Native consumers traverse
the actual HEAD/load/shared-operation/output chain without replaying requests.

Storage paging cases require complete log listings before schema selection.
Wire cases compare ordered JSON bytes, not only parsed-value equality.
Unicode retention inputs and digit limits remain explicit validation boundaries.

Reference execution uses injected HTTP, credentials, time and an in-memory
filesystem. Recorded identities are verified before executing cases.
No live service, application initialization or real credential store is used.

From `rust/`, through the applicable validation gate:

```text
python -B tests\compatibility\tables\reference.py
```

Related presentation and resource runners verify stream placement, table
headers, nested job-type selection and normalized errors. Fixtures are not
regenerated from native output; observed reports remain separate artifacts.
