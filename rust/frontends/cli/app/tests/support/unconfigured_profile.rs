// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

pub(crate) fn error_code() -> &'static str {
    if cfg!(all(
        feature = "interactive",
        not(all(windows, feature = "windows-broker"))
    )) {
        "invalid_configuration"
    } else {
        "NotConfigured"
    }
}
