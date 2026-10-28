// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::BrokerError;

/// Whether an explicit CLI request may display broker sign-in/MFA UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interaction {
    /// Never display UI; return interaction-required instead.
    Silent,
    /// Attempt silent acquisition, then interact only when required.
    Allowed,
}

/// Public-cloud resources requested separately, in Python login order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenAudience {
    /// Fabric REST APIs.
    Fabric,
    /// OneLake, using the Storage audience.
    OneLake,
    /// Azure Resource Manager.
    Arm,
}

impl TokenAudience {
    /// The exact public-cloud scope, never inferred from a user URL.
    pub const fn scope(self) -> &'static str {
        match self {
            Self::Fabric => "https://api.fabric.microsoft.com/.default",
            Self::OneLake => "https://storage.azure.com/.default",
            Self::Arm => "https://management.azure.com/.default",
        }
    }
}

/// Explicit, non-secret public-cloud configuration; constructing it does no I/O.
#[derive(Clone, Debug)]
pub struct EntraOptions {
    pub(crate) tenant: String,
    pub(crate) client: String,
    pub(crate) account: String,
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

impl EntraOptions {
    /// Validates an explicit account UPN, tenant GUID and registered public client.
    ///
    /// # Errors
    ///
    /// Returns sanitized configuration errors before any account/store access.
    pub fn new(tenant: &str, client: &str, account: &str) -> Result<Self, BrokerError> {
        if !guid(tenant)
            || !guid(client)
            || account.len() > 320
            || account.split('@').count() != 2
            || account.starts_with('@')
            || account.ends_with('@')
            || account.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(BrokerError::InvalidConfiguration);
        }
        Ok(Self {
            tenant: tenant.to_ascii_lowercase(),
            client: client.to_ascii_lowercase(),
            account: account.to_owned(),
        })
    }

    /// Explicit target tenant, not the account's inferred home tenant.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Public client registration identifier.
    pub fn client(&self) -> &str {
        &self.client
    }

    /// Explicit work-account UPN.
    pub fn account(&self) -> &str {
        &self.account
    }

    #[cfg(any(windows, test))]
    pub(crate) fn authority(&self) -> String {
        format!("https://login.microsoftonline.com/{}", self.tenant)
    }

    #[cfg(any(windows, test))]
    pub(crate) fn validate_authority(&self, value: &str) -> Result<(), BrokerError> {
        if value
            .trim_end_matches('/')
            .eq_ignore_ascii_case(&self.authority())
        {
            Ok(())
        } else {
            Err(BrokerError::IdentityMismatch)
        }
    }

    #[cfg(any(windows, test))]
    pub(crate) fn request_properties(&self, interactive: bool) -> Vec<(&'static str, String)> {
        let mut properties = vec![
            ("authority", self.authority()),
            ("validateAuthority", "yes".to_owned()),
            ("wam_compat", "2.0".to_owned()),
            ("LoginHint", self.account().to_owned()),
        ];
        if interactive {
            properties.push(("prompt", "select_account".to_owned()));
        }
        properties
    }
}

#[cfg(any(windows, test))]
pub(crate) fn needs_interaction(status: i32, interaction: Interaction) -> bool {
    status == 3 && interaction == Interaction::Allowed
}

// MSAL.NET 4.30.1 AadPlugin/CoreHelpers: TokenExpiresOn is seconds since 1601,
// not FILETIME's 100-nanosecond ticks. Never derive expiry from access-token JWTs.
#[cfg(any(windows, test))]
pub(crate) fn unix_expiry(value: &str, now: i64) -> Result<i64, BrokerError> {
    let timestamp = value
        .parse::<u64>()
        .ok()
        .and_then(|seconds| seconds.checked_sub(11_644_473_600))
        .and_then(|seconds| i64::try_from(seconds).ok())
        .filter(|seconds| *seconds > now && *seconds <= 253_402_300_799)
        .ok_or(BrokerError::InvalidResponse)?;
    Ok(timestamp)
}

#[cfg(any(windows, test))]
pub(crate) fn response_identity(
    options: &EntraOptions,
    binding: Option<&str>,
    id: &str,
    username: &str,
    authority: &str,
) -> Result<(), BrokerError> {
    if id.is_empty()
        || !username.eq_ignore_ascii_case(options.account())
        || binding.is_some_and(|expected| expected != id)
    {
        return Err(BrokerError::IdentityMismatch);
    }
    options.validate_authority(authority)
}

#[cfg(any(windows, test))]
pub(crate) fn request_status(status: i32, code: u32) -> Result<(), BrokerError> {
    match status {
        0 => Ok(()),
        1 => Err(BrokerError::Cancelled),
        2 => Err(BrokerError::IdentityMismatch),
        3 => Err(BrokerError::InteractionRequired),
        4 => Err(BrokerError::ProviderUnavailable),
        _ => Err(BrokerError::Provider { status, code }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TENANT: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn input_and_authority_fail_closed_without_network() {
        for tenant in [
            "",
            "common",
            "organizations",
            "../tenant",
            "https://evil.test",
        ] {
            assert!(EntraOptions::new(tenant, TENANT, "user@example.test").is_err());
        }
        for account in [
            "",
            "@example.test",
            "user@",
            "user@@example.test",
            "x\n@y",
            "x y@z",
        ] {
            assert!(EntraOptions::new(TENANT, TENANT, account).is_err());
        }
        let options = EntraOptions::new(TENANT, TENANT, "user@example.test").unwrap();
        assert!(
            options
                .validate_authority(&format!("{}/", options.authority()))
                .is_ok()
        );
        for authority in [
            "http://login.microsoftonline.com/11111111-2222-3333-4444-555555555555",
            "https://login.microsoftonline.com/common",
            "https://login.microsoftonline.com.evil.test/11111111-2222-3333-4444-555555555555",
        ] {
            assert_eq!(
                options.validate_authority(authority),
                Err(BrokerError::IdentityMismatch)
            );
        }
    }

    #[test]
    fn broker_expiry_uses_authoritative_seconds_and_rejects_guesses() {
        assert_eq!(unix_expiry("13344477200", 1_700_000_000), Ok(1_700_003_600));
        for invalid in [
            "",
            "0",
            "-1",
            "1700003600",
            "133444772000000000",
            "18446744073709551615",
            "not-an-expiry",
        ] {
            assert_eq!(
                unix_expiry(invalid, 1_700_000_000),
                Err(BrokerError::InvalidResponse)
            );
        }
        assert!(unix_expiry("13344477200", 1_700_003_600).is_err());
    }

    #[test]
    fn audiences_match_python_and_are_never_combined() {
        assert_eq!(
            TokenAudience::Fabric.scope(),
            "https://api.fabric.microsoft.com/.default"
        );
        assert_eq!(
            TokenAudience::OneLake.scope(),
            "https://storage.azure.com/.default"
        );
        assert_eq!(
            TokenAudience::Arm.scope(),
            "https://management.azure.com/.default"
        );
    }

    #[test]
    fn returned_identity_must_match_selected_account_tenant_and_binding() {
        let options = EntraOptions::new(TENANT, TENANT, "user@example.test").unwrap();
        let authority = options.authority();
        assert!(
            response_identity(
                &options,
                Some("account-id"),
                "account-id",
                "USER@example.test",
                &authority
            )
            .is_ok()
        );
        for (id, username, returned_authority) in [
            ("", "user@example.test", authority.as_str()),
            ("different-id", "user@example.test", authority.as_str()),
            ("account-id", "other@example.test", authority.as_str()),
            (
                "account-id",
                "user@example.test",
                "https://login.microsoftonline.com/common",
            ),
        ] {
            assert_eq!(
                response_identity(
                    &options,
                    Some("account-id"),
                    id,
                    username,
                    returned_authority
                ),
                Err(BrokerError::IdentityMismatch)
            );
        }
    }

    #[test]
    fn native_statuses_do_not_turn_cancel_switch_or_provider_failure_into_interaction() {
        assert_eq!(request_status(0, 0), Ok(()));
        assert_eq!(request_status(1, 0), Err(BrokerError::Cancelled));
        assert_eq!(request_status(2, 0), Err(BrokerError::IdentityMismatch));
        assert_eq!(request_status(3, 0), Err(BrokerError::InteractionRequired));
        assert_eq!(request_status(4, 0), Err(BrokerError::ProviderUnavailable));
        for status in [5, 99] {
            assert_eq!(
                request_status(status, 1234),
                Err(BrokerError::Provider { status, code: 1234 })
            );
        }
    }

    #[test]
    fn request_fields_pin_the_tenant_and_only_interactive_requests_select_accounts() {
        let options = EntraOptions::new(TENANT, TENANT, "user@example.test").unwrap();
        let silent = options.request_properties(false);
        assert_eq!(
            silent,
            vec![
                (
                    "authority",
                    format!("https://login.microsoftonline.com/{TENANT}")
                ),
                ("validateAuthority", "yes".into()),
                ("wam_compat", "2.0".into()),
                ("LoginHint", "user@example.test".into()),
            ]
        );
        let interactive = options.request_properties(true);
        assert_eq!(&interactive[..4], silent);
        assert_eq!(interactive[4], ("prompt", "select_account".into()));
    }

    #[test]
    fn only_a_silent_interaction_required_result_may_start_authorized_ui() {
        for status in [0, 1, 2, 3, 4, 5, 99] {
            assert!(!needs_interaction(status, Interaction::Silent));
            assert_eq!(needs_interaction(status, Interaction::Allowed), status == 3);
        }
    }
}
