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
#[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
use fabric_identity::headless::{
    CertificateConfig, Pkcs11Authentication, Pkcs11Selection, Pkcs11Signer, Pkcs11Token,
};
#[cfg(all(target_os = "macos", feature = "certificate-store-macos"))]
use fabric_identity::headless::{
    CertificateConfig,
    certificate_macos::{MacCertificateSelector, MacKeychainHelper, MacStoreSigner},
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
        let pkcs11_mode =
            cfg!(feature = "certificate-store-pkcs11") && mode == "certificate-store-pkcs11";
        let macos_mode =
            cfg!(feature = "certificate-store-macos") && mode == "certificate-store-macos";
        if !matches!(
            mode,
            "client-secret"
                | "client-assertion"
                | "workload-identity"
                | "azure-pipelines"
                | "managed-identity-vm"
                | "managed-identity-app-service"
        ) && !certificate_mode
            && !pkcs11_mode
            && !macos_mode
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
        #[cfg(not(target_os = "linux"))]
        if pkcs11_mode {
            return Err(super::native_read::failure(
                "unsupported",
                "The selected Linux PKCS#11 certificate host is unavailable.",
                1,
            ));
        }
        #[cfg(not(target_os = "macos"))]
        if macos_mode {
            return Err(super::native_read::failure(
                "unsupported",
                "The selected macOS Keychain certificate host is unavailable.",
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
            #[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
            "certificate-store-pkcs11" => {
                let signer = Self::pkcs11_signer(&read)?;
                binding = signer.binding();
                HeadlessInput::Certificate {
                    identity,
                    certificate: Box::new(CertificateConfig::new(Arc::new(signer))),
                }
            }
            #[cfg(all(target_os = "macos", feature = "certificate-store-macos"))]
            "certificate-store-macos" => {
                let fingerprint =
                    Self::certificate_fingerprint(&required("FAB_RUST_CERTIFICATE_SHA256")?)
                        .ok_or_else(invalid)?;
                let selector = MacCertificateSelector::new(
                    required("FAB_RUST_CERTIFICATE_KEYCHAIN")?,
                    fingerprint,
                )
                .map_err(|_| invalid())?;
                let helper_hash =
                    Self::certificate_fingerprint(&required("FAB_RUST_KEYCHAIN_HELPER_SHA256")?)
                        .ok_or_else(invalid)?;
                let helper = MacKeychainHelper::new(
                    std::path::PathBuf::from(required("FAB_RUST_KEYCHAIN_HELPER")?),
                    helper_hash,
                    std::time::Duration::from_secs(120),
                )
                .map_err(|_| invalid())?;
                let signer = MacStoreSigner::new(selector, helper);
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

    #[cfg(any(
        all(windows, feature = "certificate-store-windows"),
        all(target_os = "linux", feature = "certificate-store-pkcs11"),
        all(target_os = "macos", feature = "certificate-store-macos")
    ))]
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

    #[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
    fn pkcs11_signer(
        read: &impl Fn(&str) -> Option<OsString>,
    ) -> Result<Pkcs11Signer, super::native_read::Failure> {
        let invalid = |field: &str| {
            super::native_read::failure(
                "invalid_configuration",
                format!("Invalid explicit PKCS#11 configuration field: {field}."),
                2,
            )
        };
        let required = |field: &str| {
            read(field)
                .and_then(|value| value.into_string().ok())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid(field))
        };
        if required("FAB_RUST_PKCS11_PROVIDER_KIND")? != "direct" {
            return Err(invalid("FAB_RUST_PKCS11_PROVIDER_KIND"));
        }
        let module = PathBuf::from(required("FAB_RUST_PKCS11_MODULE")?);
        let module_sha256 =
            Self::certificate_fingerprint(&required("FAB_RUST_PKCS11_MODULE_SHA256")?)
                .ok_or_else(|| invalid("FAB_RUST_PKCS11_MODULE_SHA256"))?;
        let slot_text = required("FAB_RUST_PKCS11_SLOT")?;
        if slot_text.len() > 20 || !slot_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("FAB_RUST_PKCS11_SLOT"));
        }
        let slot = slot_text
            .parse::<u64>()
            .map_err(|_| invalid("FAB_RUST_PKCS11_SLOT"))?;
        let token = Pkcs11Token::new(
            &required("FAB_RUST_PKCS11_TOKEN_MANUFACTURER")?,
            &required("FAB_RUST_PKCS11_TOKEN_MODEL")?,
            &required("FAB_RUST_PKCS11_TOKEN_SERIAL")?,
        )
        .map_err(|error| invalid(&format!("{:?}", error.field)))?;
        let fingerprint = Self::certificate_fingerprint(&required("FAB_RUST_CERTIFICATE_SHA256")?)
            .ok_or_else(|| invalid("FAB_RUST_CERTIFICATE_SHA256"))?;
        let key_id = required("FAB_RUST_PKCS11_CKA_ID")?;
        if key_id.is_empty()
            || key_id.len() > 256
            || key_id.len() % 2 != 0
            || !key_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid("FAB_RUST_PKCS11_CKA_ID"));
        }
        let key_id = (0..key_id.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&key_id[index..index + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid("FAB_RUST_PKCS11_CKA_ID"))?;
        let selection =
            Pkcs11Selection::new(module, module_sha256, slot, token, fingerprint, key_id)
                .map_err(|error| invalid(&format!("{:?}", error.field)))?;
        let authentication = match required("FAB_RUST_PKCS11_AUTHENTICATION")?.as_str() {
            "pin" => {
                let pin = required("FAB_RUST_PKCS11_PIN")?;
                if pin.len() > 1024 {
                    return Err(invalid("FAB_RUST_PKCS11_PIN"));
                }
                let pin = azure_core::credentials::Secret::new(pin);
                Pkcs11Authentication::Pin(Arc::new(move || Ok(pin.clone())))
            }
            "protected-path" => {
                if read("FAB_RUST_PKCS11_PIN").is_some() {
                    return Err(invalid("FAB_RUST_PKCS11_PIN"));
                }
                Pkcs11Authentication::ProtectedPath
            }
            _ => return Err(invalid("FAB_RUST_PKCS11_AUTHENTICATION")),
        };
        Pkcs11Signer::new(
            selection,
            authentication,
            std::time::Duration::from_secs(30),
        )
        .map_err(|error| invalid(&format!("{:?}", error.field)))
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

    #[cfg(all(not(target_os = "linux"), feature = "certificate-store-pkcs11"))]
    #[test]
    fn pkcs11_mode_off_linux_does_not_read_configuration() {
        assert!(
            Selection::from_inputs("certificate-store-pkcs11", |_| {
                panic!("an unavailable platform must not inspect PKCS#11 inputs")
            })
            .is_err()
        );
    }

    #[cfg(not(feature = "certificate-store-pkcs11"))]
    #[test]
    fn disabled_pkcs11_mode_does_not_discover_inputs() {
        assert!(
            Selection::from_inputs("certificate-store-pkcs11", |_| {
                panic!("a disabled mode must not inspect PKCS#11 inputs")
            })
            .is_err()
        );
    }

    #[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
    fn pkcs11_input(name: &str) -> Option<OsString> {
        match name {
            "FAB_RUST_TENANT" => Some(TENANT.into()),
            "FAB_RUST_CLIENT_ID" => Some(CLIENT.into()),
            "FAB_RUST_PKCS11_PROVIDER_KIND" => Some("direct".into()),
            "FAB_RUST_PKCS11_MODULE" => Some("/opt/vendor/libpkcs11.so".into()),
            "FAB_RUST_PKCS11_MODULE_SHA256" => Some("ab".repeat(32).into()),
            "FAB_RUST_PKCS11_SLOT" => Some("17".into()),
            "FAB_RUST_PKCS11_TOKEN_MANUFACTURER" => Some("Synthetic vendor".into()),
            "FAB_RUST_PKCS11_TOKEN_MODEL" => Some("Test model".into()),
            "FAB_RUST_PKCS11_TOKEN_SERIAL" => Some("Test serial".into()),
            "FAB_RUST_CERTIFICATE_SHA256" => Some("cd".repeat(32).into()),
            "FAB_RUST_PKCS11_CKA_ID" => Some("0123".into()),
            "FAB_RUST_PKCS11_AUTHENTICATION" => Some("pin".into()),
            "FAB_RUST_PKCS11_PIN" => Some("synthetic-secret-pin".into()),
            _ => None,
        }
    }

    #[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
    #[test]
    fn pkcs11_capture_is_inert_and_binds_all_public_selection_fields() {
        let captured = Selection::from_inputs("certificate-store-pkcs11", pkcs11_input).unwrap();
        assert_eq!(captured.client, CLIENT);
        assert!(!captured.binding.contains("synthetic-secret-pin"));
        assert!(captured.metadata_endpoint().is_none());
        for (field, value) in [
            ("FAB_RUST_PKCS11_MODULE", "/opt/other/libpkcs11.so"),
            ("FAB_RUST_PKCS11_MODULE_SHA256", &"ef".repeat(32)),
            ("FAB_RUST_PKCS11_SLOT", "18"),
            ("FAB_RUST_PKCS11_TOKEN_MANUFACTURER", "Other vendor"),
            ("FAB_RUST_PKCS11_TOKEN_MODEL", "Other model"),
            ("FAB_RUST_PKCS11_TOKEN_SERIAL", "Other serial"),
            ("FAB_RUST_CERTIFICATE_SHA256", &"ef".repeat(32)),
            ("FAB_RUST_PKCS11_CKA_ID", "0124"),
        ] {
            let rotated = Selection::from_inputs("certificate-store-pkcs11", |name| {
                if name == field {
                    Some(value.into())
                } else {
                    pkcs11_input(name)
                }
            })
            .unwrap();
            assert_ne!(captured.binding, rotated.binding, "{field}");
        }
    }

    #[cfg(all(target_os = "linux", feature = "certificate-store-pkcs11"))]
    #[test]
    fn pkcs11_capture_rejects_missing_selectors_and_ambiguous_pin_policy() {
        for missing in [
            "FAB_RUST_PKCS11_PROVIDER_KIND",
            "FAB_RUST_PKCS11_MODULE",
            "FAB_RUST_PKCS11_MODULE_SHA256",
            "FAB_RUST_PKCS11_SLOT",
            "FAB_RUST_PKCS11_TOKEN_MANUFACTURER",
            "FAB_RUST_PKCS11_TOKEN_MODEL",
            "FAB_RUST_PKCS11_TOKEN_SERIAL",
            "FAB_RUST_CERTIFICATE_SHA256",
            "FAB_RUST_PKCS11_CKA_ID",
            "FAB_RUST_PKCS11_AUTHENTICATION",
            "FAB_RUST_PKCS11_PIN",
        ] {
            assert!(
                Selection::from_inputs("certificate-store-pkcs11", |name| {
                    if name == missing {
                        None
                    } else {
                        pkcs11_input(name)
                    }
                })
                .is_err(),
                "{missing}"
            );
        }
        assert!(
            Selection::from_inputs("certificate-store-pkcs11", |name| {
                if name == "FAB_RUST_PKCS11_AUTHENTICATION" {
                    Some("protected-path".into())
                } else {
                    pkcs11_input(name)
                }
            })
            .is_err()
        );
        let protected = Selection::from_inputs("certificate-store-pkcs11", |name| match name {
            "FAB_RUST_PKCS11_AUTHENTICATION" => Some("protected-path".into()),
            "FAB_RUST_PKCS11_PIN" => None,
            _ => pkcs11_input(name),
        })
        .unwrap();
        assert!(protected.binding.ends_with(":protected-path"));
    }
}
