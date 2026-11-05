// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Binary-private explicit environment selection, separate from credential APIs.

#[cfg(all(windows, feature = "certificate-store-windows"))]
use fabric_identity::headless::{
    AllowedCertificateProvider, CertificateConfig, CertificateSelector, CertificateStore,
    WindowsStoreSigner,
};
use fabric_identity::headless::{
    AssertionFile, HeadlessCredential, HeadlessInput, ManagedHost, PipelinesConfig, ServiceIdentity,
};
use std::{ffi::OsString, path::PathBuf, sync::Arc};

pub(crate) struct Selection {
    pub(crate) tenant: String,
    pub(crate) account: String,
    pub(crate) client: String,
    pub(crate) binding: String,
    input: HeadlessInput,
}

impl Selection {
    pub(crate) fn capture(mode: &str) -> Result<Self, super::native_read::Failure> {
        Self::from_inputs(mode, |name| std::env::var_os(name))
    }

    pub(crate) fn from_inputs(
        mode: &str,
        read: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, super::native_read::Failure> {
        let invalid = || {
            super::native_read::failure(
                "invalid_configuration",
                "Invalid explicit headless credential configuration.",
                2,
            )
        };
        let required = |name: &str| {
            read(name)
                .and_then(|value| value.into_string().ok())
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(invalid)
        };
        let alias = |primary: &str, secondary: &str| {
            let primary = read(primary);
            let secondary = read(secondary);
            let selected = match (primary, secondary) {
                (Some(left), Some(right)) if left != right => return Err(invalid()),
                (Some(value), _) | (_, Some(value)) => value,
                _ => return Err(invalid()),
            };
            selected
                .into_string()
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(invalid)
        };
        let certificate_mode =
            cfg!(feature = "certificate-store-windows") && mode == "certificate-store-windows";
        if !matches!(
            mode,
            "client-secret"
                | "client-assertion"
                | "workload-identity"
                | "azure-pipelines"
                | "managed-identity-vm"
                | "managed-identity-app-service"
        ) && !certificate_mode
        {
            return Err(invalid());
        }
        #[cfg(not(windows))]
        if certificate_mode {
            return Err(super::native_read::failure(
                "unsupported",
                "The selected Windows certificate-store host is unavailable.",
                1,
            ));
        }
        let tenant = alias("FAB_RUST_TENANT", "AZURE_TENANT_ID")?;
        let managed = mode.starts_with("managed-identity-");
        let client = if managed {
            match required("FAB_RUST_MANAGED_IDENTITY")?.as_str() {
                "system" => String::new(),
                "client" => alias("FAB_RUST_CLIENT_ID", "AZURE_CLIENT_ID")?,
                _ => return Err(invalid()),
            }
        } else {
            alias("FAB_RUST_CLIENT_ID", "AZURE_CLIENT_ID")?
        };
        // Validate tenant even for a host-assigned identity; this remains a label,
        // not an assertion that managed identity token claims were verified.
        let identity = ServiceIdentity::new(
            &tenant,
            if client.is_empty() {
                "00000000-0000-0000-0000-000000000000"
            } else {
                &client
            },
        )
        .map_err(|_| invalid())?;
        let mut binding = mode.to_owned();
        let input = match mode {
            "client-secret" => HeadlessInput::ClientSecret {
                identity,
                secret: azure_core::credentials::Secret::new(required("FAB_RUST_CLIENT_SECRET")?),
            },
            "client-assertion" | "workload-identity" => {
                let path = if mode == "workload-identity" {
                    alias("FAB_RUST_ASSERTION_FILE", "AZURE_FEDERATED_TOKEN_FILE")?
                } else {
                    required("FAB_RUST_ASSERTION_FILE")?
                };
                let assertion = AssertionFile::new(PathBuf::from(&path)).map_err(|_| invalid())?;
                binding.push(':');
                binding.push_str(&path);
                HeadlessInput::AssertionFile {
                    identity,
                    assertion,
                }
            }
            "azure-pipelines" => {
                let pipeline = PipelinesConfig::from_inputs(
                    &required("FAB_RUST_PIPELINES_SERVICE_CONNECTION_ID")?,
                    &required("FAB_RUST_PIPELINES_OIDC_URI")?,
                    azure_core::credentials::Secret::new(required("SYSTEM_ACCESSTOKEN")?),
                    &read,
                )
                .map_err(|_| invalid())?;
                binding = pipeline.binding();
                HeadlessInput::AzurePipelines {
                    identity,
                    pipeline: Box::new(pipeline),
                }
            }
            #[cfg(all(windows, feature = "certificate-store-windows"))]
            "certificate-store-windows" => {
                let store = match required("FAB_RUST_CERTIFICATE_STORE")?.as_str() {
                    "current-user-my" => CertificateStore::CurrentUserMy,
                    "local-machine-my" => CertificateStore::LocalMachineMy,
                    _ => return Err(invalid()),
                };
                let text = required("FAB_RUST_CERTIFICATE_SHA256")?;
                let sha256 = Self::certificate_fingerprint(&text).ok_or_else(invalid)?;
                if required("FAB_RUST_CERTIFICATE_PROVIDER")? != "microsoft-software" {
                    return Err(invalid());
                }
                let selector = CertificateSelector::new(
                    store,
                    sha256,
                    AllowedCertificateProvider::MicrosoftSoftware,
                );
                let signer = WindowsStoreSigner::new(selector);
                binding = signer.binding();
                HeadlessInput::Certificate {
                    identity,
                    certificate: Box::new(CertificateConfig::new(Arc::new(signer))),
                }
            }
            "managed-identity-vm" | "managed-identity-app-service" => {
                let host = if mode == "managed-identity-vm" {
                    ManagedHost::VirtualMachine
                } else {
                    let endpoint = required("IDENTITY_ENDPOINT")?;
                    binding.push(':');
                    binding.push_str(&endpoint);
                    required("IDENTITY_HEADER")?;
                    ManagedHost::app_service(&endpoint).map_err(|_| invalid())?
                };
                HeadlessInput::ManagedIdentity {
                    host,
                    client_id: (!client.is_empty()).then(|| client.clone()),
                }
            }
            _ => return Err(invalid()),
        };
        let account = if client.is_empty() {
            "system-assigned".into()
        } else {
            client.clone()
        };
        Ok(Self {
            tenant,
            account,
            client,
            binding,
            input,
        })
    }

    #[cfg(all(windows, feature = "certificate-store-windows"))]
    fn certificate_fingerprint(value: &str) -> Option<[u8; 32]> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let mut fingerprint = [0; 32];
        for (index, byte) in fingerprint.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(fingerprint)
    }

    pub(crate) fn credential(
        self,
        http: Arc<dyn azure_core::http::HttpClient>,
    ) -> Result<Arc<dyn azure_core::credentials::TokenCredential>, super::native_read::Failure>
    {
        HeadlessCredential::new(self.input, http)
            .map(|credential| credential as Arc<dyn azure_core::credentials::TokenCredential>)
            .map_err(|_| {
                super::native_read::failure(
                    "invalid_configuration",
                    "Invalid explicit headless credential or selected provider source contract.",
                    2,
                )
            })
    }

    pub(crate) fn metadata_endpoint(&self) -> Option<azure_core::http::Url> {
        match &self.input {
            HeadlessInput::ManagedIdentity { host, .. } => Some(host.endpoint()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TENANT: &str = "11111111-1111-1111-1111-111111111111";
    const CLIENT: &str = "22222222-2222-2222-2222-222222222222";

    fn input(name: &str) -> Option<OsString> {
        match name {
            "FAB_RUST_TENANT" => Some(TENANT.into()),
            "FAB_RUST_CLIENT_ID" => Some(CLIENT.into()),
            "FAB_RUST_CLIENT_SECRET" => Some("synthetic-secret".into()),
            _ => None,
        }
    }

    #[test]
    fn secret_presence_never_selects_a_mode_or_account() {
        assert!(Selection::from_inputs("", input).is_err());
        assert!(Selection::from_inputs("unknown-sensitive-value", input).is_err());
        let selection = Selection::from_inputs("client-secret", input).unwrap();
        assert_eq!(selection.account, CLIENT);
        assert_eq!(selection.binding, "client-secret");
        assert!(!selection.binding.contains("synthetic-secret"));
    }

    #[test]
    fn provider_selection_does_not_read_unselected_secret_or_host_inputs() {
        let selection = Selection::from_inputs("client-secret", |name| {
            assert!(!name.starts_with("IDENTITY_"));
            assert!(!name.contains("ASSERTION") && !name.contains("FEDERATED"));
            input(name)
        })
        .unwrap();
        assert_eq!(selection.tenant, TENANT);
        assert_eq!(selection.client, CLIENT);
    }

    #[test]
    fn conflicting_aliases_and_implicit_managed_identity_fail_closed() {
        assert!(
            Selection::from_inputs("client-secret", |name| {
                if name == "AZURE_CLIENT_ID" {
                    Some(TENANT.into())
                } else {
                    input(name)
                }
            })
            .is_err()
        );
        assert!(Selection::from_inputs("managed-identity-vm", input).is_err());
        let selection = Selection::from_inputs("managed-identity-vm", |name| {
            if name == "FAB_RUST_MANAGED_IDENTITY" {
                Some("system".into())
            } else if name == "FAB_RUST_CLIENT_SECRET" {
                panic!("unselected secret")
            } else {
                input(name)
            }
        })
        .unwrap();
        assert_eq!(selection.account, "system-assigned");
        assert!(selection.client.is_empty());
    }

    fn pipeline_input(name: &str) -> Option<OsString> {
        match name {
            "FAB_RUST_PIPELINES_SERVICE_CONNECTION_ID" => {
                Some("33333333-3333-3333-3333-333333333333".into())
            }
            "FAB_RUST_PIPELINES_OIDC_URI" | "SYSTEM_OIDCREQUESTURI" => Some(format!(
                "https://dev.azure.com/organization/{TENANT}/_apis/distributedtask/hubs/build/plans/{TENANT}/jobs/{CLIENT}/oidctoken"
            ).into()),
            "SYSTEM_COLLECTIONURI" => Some("https://dev.azure.com/organization/".into()),
            "SYSTEM_TEAMPROJECTID" | "SYSTEM_PLANID" => Some(TENANT.into()),
            "SYSTEM_JOBID" => Some(CLIENT.into()),
            "SYSTEM_HOSTTYPE" => Some("build".into()),
            "SYSTEM_ACCESSTOKEN" => Some("synthetic-system-token".into()),
            _ => {
                assert!(name != "FAB_RUST_CLIENT_SECRET", "unselected secret");
                assert!(!name.starts_with("IDENTITY_"), "unselected host");
                assert!(!name.contains("ASSERTION"), "unselected file");
                input(name)
            }
        }
    }

    #[test]
    fn pipelines_binding_is_explicit_connection_bound_and_secret_free() {
        let selection = Selection::from_inputs("azure-pipelines", pipeline_input).unwrap();
        assert_eq!(selection.account, CLIENT);
        assert!(selection.binding.starts_with("azure-pipelines:33333333-"));
        assert!(!selection.binding.contains("synthetic"));
        assert!(selection.metadata_endpoint().is_none());
        let changed_connection = Selection::from_inputs("azure-pipelines", |name| {
            if name == "FAB_RUST_PIPELINES_SERVICE_CONNECTION_ID" {
                Some(TENANT.into())
            } else {
                pipeline_input(name)
            }
        })
        .unwrap();
        assert_ne!(selection.binding, changed_connection.binding);
        let rotated_token = Selection::from_inputs("azure-pipelines", |name| {
            if name == "SYSTEM_ACCESSTOKEN" {
                Some("synthetic-rotated-token".into())
            } else {
                pipeline_input(name)
            }
        })
        .unwrap();
        assert_eq!(selection.binding, rotated_token.binding);
        for missing in [
            "SYSTEM_ACCESSTOKEN",
            "FAB_RUST_PIPELINES_SERVICE_CONNECTION_ID",
            "FAB_RUST_PIPELINES_OIDC_URI",
            "SYSTEM_JOBID",
        ] {
            assert!(
                Selection::from_inputs("azure-pipelines", |name| {
                    if name == missing {
                        None
                    } else {
                        pipeline_input(name)
                    }
                })
                .is_err()
            );
        }
        assert!(
            Selection::from_inputs("azure-pipelines", |name| {
                if name == "SYSTEM_JOBID" {
                    Some(TENANT.into())
                } else {
                    pipeline_input(name)
                }
            })
            .is_err()
        );
    }

    #[cfg(all(windows, feature = "certificate-store-windows"))]
    fn certificate_input(name: &str) -> Option<OsString> {
        assert!(!name.contains("SECRET") && !name.contains("PASSWORD"));
        assert!(!name.contains("ASSERTION") && !name.contains("FEDERATED"));
        assert!(!name.starts_with("IDENTITY_") && !name.starts_with("SYSTEM_"));
        match name {
            "FAB_RUST_TENANT" => Some(TENANT.into()),
            "FAB_RUST_CLIENT_ID" => Some(CLIENT.into()),
            "FAB_RUST_CERTIFICATE_STORE" => Some("current-user-my".into()),
            "FAB_RUST_CERTIFICATE_SHA256" => Some("ab".repeat(32).into()),
            "FAB_RUST_CERTIFICATE_PROVIDER" => Some("microsoft-software".into()),
            _ => None,
        }
    }

    #[cfg(all(windows, feature = "certificate-store-windows"))]
    #[test]
    fn certificate_selection_captures_only_explicit_nonsecret_metadata() {
        let selection =
            Selection::from_inputs("certificate-store-windows", certificate_input).unwrap();
        assert_eq!(selection.tenant, TENANT);
        assert_eq!(selection.account, CLIENT);
        assert_eq!(selection.client, CLIENT);
        assert_eq!(
            selection.binding,
            format!(
                "windows-certificate-store:current-user-my:microsoft-software:{}",
                "ab".repeat(32)
            )
        );
        assert!(selection.metadata_endpoint().is_none());
        let rotated = Selection::from_inputs("certificate-store-windows", |name| {
            if name == "FAB_RUST_CERTIFICATE_SHA256" {
                Some("cd".repeat(32).into())
            } else {
                certificate_input(name)
            }
        })
        .unwrap();
        assert_ne!(selection.binding, rotated.binding);
    }

    #[cfg(all(windows, feature = "certificate-store-windows"))]
    #[test]
    fn certificate_selection_rejects_missing_ambiguous_or_unapproved_inputs() {
        for missing in [
            "FAB_RUST_CERTIFICATE_STORE",
            "FAB_RUST_CERTIFICATE_SHA256",
            "FAB_RUST_CERTIFICATE_PROVIDER",
        ] {
            assert!(
                Selection::from_inputs("certificate-store-windows", |name| {
                    if name == missing {
                        None
                    } else {
                        certificate_input(name)
                    }
                })
                .is_err()
            );
        }
        for (input_name, invalid_value) in [
            ("FAB_RUST_CERTIFICATE_STORE", "sensitive-other-store"),
            ("FAB_RUST_CERTIFICATE_SHA256", "sensitive-fingerprint"),
            ("FAB_RUST_CERTIFICATE_PROVIDER", "microsoft-platform"),
            ("FAB_RUST_CERTIFICATE_PROVIDER", "microsoft-smart-card"),
        ] {
            assert!(
                Selection::from_inputs("certificate-store-windows", |name| {
                    if name == input_name {
                        Some(invalid_value.into())
                    } else {
                        certificate_input(name)
                    }
                })
                .is_err()
            );
        }
        for fingerprint in ["", "AA", &"g".repeat(64), &"ab".repeat(33)] {
            assert!(Selection::certificate_fingerprint(fingerprint).is_none());
        }
        assert_eq!(
            Selection::certificate_fingerprint(&"AB".repeat(32)),
            Some([0xab; 32])
        );
    }

    #[cfg(all(not(windows), feature = "certificate-store-windows"))]
    #[test]
    fn certificate_mode_off_windows_never_reads_selectors_or_searches_another_host() {
        assert!(
            Selection::from_inputs("certificate-store-windows", |_| {
                panic!("an unavailable platform must not inspect certificate configuration")
            })
            .is_err()
        );
    }

    #[cfg(not(feature = "certificate-store-windows"))]
    #[test]
    fn plain_headless_build_never_discovers_certificate_inputs() {
        assert!(
            Selection::from_inputs("certificate-store-windows", |_| {
                panic!("disabled certificate mode must not inspect inputs")
            })
            .is_err()
        );
    }
}
