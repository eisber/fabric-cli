// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Compile/run probe for a consumer that forbids unsafe code, with either feature set.

#![forbid(unsafe_code)]

use fabric_core::{Audience, CredentialSelection, IdentityError};

fn main() -> Result<(), IdentityError> {
    let selection = CredentialSelection::new("test-account", "test-tenant", Audience::Fabric)?;
    assert!(matches!(
        fabric_identity::interactive_credential(&selection),
        Err(IdentityError::Unsupported)
    ));
    #[cfg(all(windows, feature = "windows-broker"))]
    {
        use fabric_identity::windows_broker::{
            BrokerRequest, WebAccount, WebTokenRequest, request_interactive,
        };
        let _safe_entry: fn(WebTokenRequest, Option<WebAccount>) -> BrokerRequest =
            request_interactive;
        fn assert_send<T: Send>() {}
        assert_send::<BrokerRequest>();
    }
    Ok(())
}
