// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional identity implementations, independent of the service SDK.
//!
//! Default builds do not discover credentials or launch UI. Windows broker login
//! is explicitly opt-in; it uses no developer-tool fallback or plaintext cache.

#[cfg(all(windows, feature = "windows-broker"))]
mod windows;
#[cfg(all(windows, feature = "windows-broker"))]
pub use windows::{LoginOptions, TokenMetadata, WindowsCredential, sign_out};

/// Opt-in desktop WAM interop; not a configured Fabric credential provider.
///
/// The default build excludes this dependency. All native unsafe code lives in
/// that separate crate, not in this crate or the service SDK.
#[cfg(all(windows, feature = "windows-broker"))]
pub use fabric_identity_windows as windows_broker;

use fabric_core::{CredentialSelection, IdentityError, TokenCredential};
use std::sync::Arc;

/// Rejects interactive credential creation until a supported backend is selected.
///
/// This function does not inspect the environment, prompt, or persist anything.
/// The selection remains explicit even though no backend can currently satisfy it.
///
/// # Errors
///
/// Always returns [`IdentityError::Unsupported`]; never substitutes another identity.
pub fn interactive_credential(
    _selection: &CredentialSelection,
) -> Result<Arc<dyn TokenCredential>, IdentityError> {
    Err(IdentityError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_core::Audience;

    #[test]
    fn interactive_backend_is_explicitly_unsupported() {
        let selection =
            CredentialSelection::new("test-account", "test-tenant", Audience::Fabric).unwrap();
        assert!(matches!(
            interactive_credential(&selection),
            Err(IdentityError::Unsupported)
        ));
    }
}
