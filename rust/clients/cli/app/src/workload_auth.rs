// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! CLI-only public-client policy for the exact public Kusto workload audience.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::{Error, ErrorKind},
};
use fabric_identity::PUBLIC_KUSTO_SCOPE;
use std::sync::Arc;

/// Azure CLI's public application, selected only for delegated Kusto requests.
pub const KUSTO_CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";

#[derive(Debug)]
struct WorkloadCredential {
    selected: Arc<dyn TokenCredential>,
    kusto: Arc<dyn TokenCredential>,
}

#[async_trait]
impl TokenCredential for WorkloadCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if scopes == [PUBLIC_KUSTO_SCOPE] {
            self.kusto.get_token(scopes, options).await
        } else {
            self.selected.get_token(scopes, options).await
        }
    }
}

#[cfg(all(windows, feature = "windows-broker"))]
#[derive(Debug)]
struct BrokerKustoCredential(Arc<fabric_identity::WindowsCredential>);

#[cfg(all(windows, feature = "windows-broker"))]
#[async_trait]
impl TokenCredential for BrokerKustoCredential {
    async fn get_token(
        &self,
        _scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .get_token_for_client(
                KUSTO_CLIENT_ID,
                fabric_identity::windows_broker::TokenAudience::Kusto,
            )
            .await
            .map_err(|error| Error::new(ErrorKind::Credential, error))
    }
}

/// Retain the selected broker session, routing only Kusto through Azure CLI's app.
///
/// Construction performs no I/O. Acquisition remains silent, pins the original
/// native account and tenant, and participates in the original logout lifecycle.
/// Other audiences, including Fabric/AIHub, keep the selected application.
#[cfg(all(windows, feature = "windows-broker"))]
pub fn broker(
    selected: Arc<fabric_identity::WindowsCredential>,
) -> Arc<dyn TokenCredential> {
    Arc::new(WorkloadCredential {
        kusto: Arc::new(BrokerKustoCredential(Arc::clone(&selected))),
        selected,
    })
}

#[cfg(any(feature = "interactive", test))]
#[derive(Debug)]
struct DeviceCodeKustoCredential {
    selected: Arc<dyn TokenCredential>,
    client_matches: bool,
}

#[cfg(any(feature = "interactive", test))]
#[async_trait]
impl TokenCredential for DeviceCodeKustoCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if !self.client_matches {
            return Err(Error::with_message(
                ErrorKind::Credential,
                "Kusto requires a separately signed-in Azure CLI application session \
                 (client 04b07795-8ddb-461a-bbee-02f9e1bf7b46). The selected device-code \
                 session uses another application; no refresh or automatic sign-in \
                 was attempted. Explicitly select an Azure CLI application session \
                 before retrying; do not reuse another application's refresh token.",
            ));
        }
        self.selected.get_token(scopes, options).await
    }
}

/// Keep a selected device-code session without repurposing its refresh token.
///
/// Only a session already bound to Azure CLI's application may request Kusto.
/// A different application fails before refresh; no other session is discovered,
/// overwritten or signed in. Other audiences retain their existing behavior.
#[cfg(feature = "interactive")]
pub fn device_code(
    selected: Arc<fabric_identity::interactive::InteractiveCredential>,
) -> Arc<dyn TokenCredential> {
    let client_matches = selected.client_id().eq_ignore_ascii_case(KUSTO_CLIENT_ID);
    let selected: Arc<dyn TokenCredential> = selected;
    Arc::new(WorkloadCredential {
        kusto: Arc::new(DeviceCodeKustoCredential {
            selected: Arc::clone(&selected),
            client_matches,
        }),
        selected,
    })
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use azure_core::time::OffsetDateTime;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct Credential {
        calls: Mutex<Vec<Vec<String>>>,
        fail: bool,
    }

    #[async_trait]
    impl TokenCredential for Credential {
        async fn get_token(
            &self,
            scopes: &[&str],
            _options: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            self.calls
                .lock()
                .unwrap()
                .push(scopes.iter().map(|scope| (*scope).into()).collect());
            if self.fail {
                return Err(Error::with_message(ErrorKind::Credential, "fixture failure"));
            }
            Ok(AccessToken::new(
                "offline-fixture",
                OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap(),
            ))
        }
    }

    #[tokio::test]
    async fn only_exact_singleton_kusto_scope_selects_workload_credential() {
        let selected = Arc::new(Credential::default());
        let kusto = Arc::new(Credential::default());
        let credential = WorkloadCredential {
            selected: selected.clone(),
            kusto: kusto.clone(),
        };
        credential.get_token(&[PUBLIC_KUSTO_SCOPE], None).await.unwrap();
        for scopes in [
            vec!["https://api.fabric.microsoft.com/.default"],
            vec!["https://storage.azure.com/.default"],
            vec!["https://management.azure.com/.default"],
            vec!["https://database.windows.net/.default"],
            vec!["https://petrol.office.microsoft.com/Feedback.ReadWrite"],
            vec![PUBLIC_KUSTO_SCOPE, "https://api.fabric.microsoft.com/.default"],
            vec!["https://cluster.kusto.windows.net/.default"],
            vec![" https://api.kusto.windows.net/.default"],
            vec![],
        ] {
            credential.get_token(&scopes, None).await.unwrap();
        }
        assert_eq!(*kusto.calls.lock().unwrap(), vec![vec![PUBLIC_KUSTO_SCOPE]]);
        assert_eq!(selected.calls.lock().unwrap().len(), 9);
    }

    #[tokio::test]
    async fn kusto_failure_never_retries_selected_application() {
        let selected = Arc::new(Credential::default());
        let kusto = Arc::new(Credential {
            fail: true,
            ..Credential::default()
        });
        let credential = WorkloadCredential {
            selected: selected.clone(),
            kusto: kusto.clone(),
        };
        assert!(credential.get_token(&[PUBLIC_KUSTO_SCOPE], None).await.is_err());
        assert!(selected.calls.lock().unwrap().is_empty());
        assert_eq!(kusto.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn device_code_mismatch_never_refreshes_and_preserves_fabric() {
        let selected = Arc::new(Credential::default());
        let credential = WorkloadCredential {
            selected: selected.clone(),
            kusto: Arc::new(DeviceCodeKustoCredential {
                selected: selected.clone(),
                client_matches: false,
            }),
        };
        let error = credential.get_token(&[PUBLIC_KUSTO_SCOPE], None).await.unwrap_err();
        assert!(error.to_string().contains("no refresh or automatic sign-in"));
        assert!(selected.calls.lock().unwrap().is_empty());
        credential
            .get_token(&["https://api.fabric.microsoft.com/.default"], None)
            .await
            .unwrap();
        assert_eq!(selected.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn device_code_already_bound_to_kusto_client_delegates() {
        let selected = Arc::new(Credential::default());
        let credential = DeviceCodeKustoCredential {
            selected: selected.clone(),
            client_matches: true,
        };
        credential.get_token(&[PUBLIC_KUSTO_SCOPE], None).await.unwrap();
        assert_eq!(*selected.calls.lock().unwrap(), vec![vec![PUBLIC_KUSTO_SCOPE]]);
    }
}
