<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Output contracts

Fixed synthetic fixtures cover structured envelopes, raw JSON definitions,
text tables, notices, truncation, key/value layouts and error presentation.
Assertions preserve field order, omission, scalar types, exact bytes, streams,
newlines and exit codes.

The reference validates recorded inputs and uses frozen time, private writers
and injected terminal capabilities. It does not authenticate, access services,
mutate global application state or write baselines.

From `rust/`, within the applicable shared-host validation grant:

```text
python -B tests\compatibility\output\reference.py
python -B -m unittest discover -s tests\compatibility\output
cargo test --offline --locked -p fabric-cli-output -- --test-threads=1
cargo run --offline --locked -q -p fabric-cli-output --example compatibility_probe
```

Save native probe output to a caller-owned UTF-8 artifact and supply it with
`--observed <artifact>`. References independently compare expectations; they
never rewrite them from the probe.

Definition, Unicode, extended-value and text runners exercise distinct
serialization policies. Negative tests detect changed streams, exits, field
presence and LF/CRLF corruption. Native writer tests retain partial failures
without fallback or operation replay.
