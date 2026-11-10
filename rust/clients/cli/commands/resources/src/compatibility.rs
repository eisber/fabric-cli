// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/// Compatibility namespace for the original resource-command adapter.
///
/// New consumers can use [`crate::parser`] and [`crate::host`] directly.
pub mod frontdoor {
    pub use crate::host::*;
}
