// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private v2 composition of existing explicit headless and silent WAM hosts.

use super::{Failure, failed, headless};
use std::sync::Arc;

pub(super) enum Selection {
    Headless(headless::Selection),
    #[cfg(all(windows, feature = "windows-broker"))]
    Broker(fabric_identity::LoginOptions),
}

impl Selection {
    pub(super) fn capture() -> Result<Self, Failure> {
        let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|error| {
            if matches!(error, std::env::VarError::NotPresent) {
                failed(
                    "NotConfigured",
                    "Select an explicit FAB_RUST_AUTH_MODE; no credential chain is used.",
                    1,
                )
            } else {
                failed(
                    "InvalidConfiguration",
                    "Invalid explicit credential mode.",
                    2,
                )
            }
        })?;
        Self::from_mode(&mode, || {
            #[cfg(all(windows, feature = "windows-broker"))]
            {
                crate::auth_binding::selected()
                    .map(Self::Broker)
                    .map_err(|(code, message)| {
                        failed(
                            code,
                            message,
                            if code == "invalid_configuration" {
                                2
                            } else {
                                1
                            },
                        )
                    })
            }
            #[cfg(not(all(windows, feature = "windows-broker")))]
            {
                Err(failed(
                    "UnsupportedCredential",
                    "WAM requires Windows and the existing native-read/windows-broker feature.",
                    1,
                ))
            }
        })
    }

    fn from_mode(
        mode: &str,
        broker: impl FnOnce() -> Result<Self, Failure>,
    ) -> Result<Self, Failure> {
        if mode == "broker" {
            broker()
        } else {
            headless::Selection::capture(mode)
                .map(Self::Headless)
                .map_err(|(code, message, exit)| failed(code, message, exit))
        }
    }

    pub(super) fn tenant(&self) -> &str {
        match self {
            Self::Headless(selection) => &selection.tenant,
            #[cfg(all(windows, feature = "windows-broker"))]
            Self::Broker(options) => options.tenant(),
        }
    }

    pub(super) fn account(&self) -> &str {
        match self {
            Self::Headless(selection) => &selection.account,
            #[cfg(all(windows, feature = "windows-broker"))]
            Self::Broker(options) => options.account(),
        }
    }

    pub(super) fn metadata_endpoint(&self) -> Option<azure_core::http::Url> {
        match self {
            Self::Headless(selection) => selection.metadata_endpoint(),
            #[cfg(all(windows, feature = "windows-broker"))]
            Self::Broker(_) => None,
        }
    }

    pub(super) async fn credential(
        self,
        http: Arc<dyn azure_core::http::HttpClient>,
    ) -> Result<Arc<dyn azure_core::credentials::TokenCredential>, Failure> {
        match self {
            Self::Headless(selection) => selection
                .credential(http)
                .map_err(|(code, message, exit)| failed(code, message, exit)),
            #[cfg(all(windows, feature = "windows-broker"))]
            Self::Broker(options) => fabric_identity::WindowsCredential::connect(options)
                .await
                .map(|credential| credential as Arc<dyn azure_core::credentials::TokenCredential>)
                .map_err(|error| {
                    let (code, message) = crate::auth_binding::broker_error(error);
                    failed(code, message, 1)
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    #[test]
    fn broker_selection_failure_never_falls_back_to_another_provider() {
        let result = Selection::from_mode("broker", || {
            Err(failed("selected-broker-failure", "inert", 1))
        });
        assert!(matches!(result, Err(("selected-broker-failure", _, _, 1))));
    }

    #[test]
    fn invalid_mode_never_tries_broker_or_discovers_an_account() {
        for mode in ["", "wam", "auto", "unknown"] {
            let result = Selection::from_mode(mode, || panic!("implicit broker selection"));
            assert!(matches!(result, Err(("invalid_configuration", _, _, 2))));
        }
    }

    #[cfg(all(windows, feature = "windows-broker"))]
    #[test]
    fn broker_selection_retains_explicit_identity_without_acquiring_tokens() {
        let options = fabric_identity::LoginOptions::new(
            "11111111-2222-3333-4444-555555555555",
            fabric_cli::parser::DEFAULT_BROKER_CLIENT_ID,
            "selected@example.test",
        )
        .unwrap();
        let selection = Selection::from_mode("broker", || Ok(Selection::Broker(options.clone())))
            .unwrap_or_else(|_| panic!("valid explicit selection"));
        assert_eq!(selection.tenant(), options.tenant());
        assert_eq!(selection.account(), options.account());
        assert!(selection.metadata_endpoint().is_none());
    }
}
