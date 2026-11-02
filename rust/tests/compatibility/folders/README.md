<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Folder SDK contracts

Synthetic LIST/GET cases verify exact routes, ordered query replacement and
encoding, recursive/root option presence, token precedence and response rows.
Reference and native consumers compare the same immutable expectations.

The reference verifies input digests before guarded execution. Credentials,
HTTP, time, context and logging are injected. Real file/network/process access,
sleeping and ambient configuration are forbidden.

From `rust/`:

```text
python -B tests\compatibility\folders\reference.py --check
python -B -m unittest discover -s tests\compatibility\folders -p test_reference.py
cargo test --offline --locked -p fabric-sdk-folders
cargo test --offline --locked -p fabric-sdk --test folder_facade
cargo test --offline --locked -p fabric-sdk-http --test folder_adapter
```

Negative cases enforce malformed-response errors, active origin/route checks,
cycle detection, byte/page/row limits and page-atomic progress. Cancellation
and later-page failures remain explicit.

Folder tree assembly and command formatting are tested separately. A passing
raw SDK case does not establish the entire command or live authorization.
