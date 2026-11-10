// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Test-only artifact selection; never used by production update code.

use std::path::PathBuf;

/// Prefer an explicit runner artifact root; ordinary test users use OS scratch.
pub(super) fn test_root() -> PathBuf {
    let root = std::env::var_os("FAB_UPDATE_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    assert!(
        root.is_absolute(),
        "the test artifact root must be absolute"
    );
    root
}
