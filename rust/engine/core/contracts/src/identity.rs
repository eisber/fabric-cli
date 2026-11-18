// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CoreError, ErrorKind};
use serde::{Deserialize, Serialize};

/// Non-secret identity consistency metadata, not cryptographic token verification.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Entra tenant object ID.
    pub tenant: String,
    /// Entra principal object ID. Subscription is deliberately not selected.
    pub principal: String,
}

impl Binding {
    /// Validate persisted metadata without accepting incomplete bindings.
    pub fn validate(&self) -> Result<(), CoreError> {
        fn valid_id(id: &str) -> bool {
            id.len() == 36
                && id.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
        }

        if valid_id(&self.tenant) && valid_id(&self.principal) {
            Ok(())
        } else {
            Err(CoreError::new(
                ErrorKind::Unauthorized,
                AZURE_CLI_LOGIN_REQUIRED,
            ))
        }
    }
}

/// Stable diagnostic for a missing or changed Azure CLI identity binding.
pub const AZURE_CLI_LOGIN_REQUIRED: &str = "Azure CLI identity unavailable or changed. Install Azure CLI 2.54+ and run az login, then fab-rs auth login";
