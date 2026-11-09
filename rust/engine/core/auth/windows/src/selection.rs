// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![forbid(unsafe_code)]

use crate::{
    BrokerError, EntraOptions, Interaction, ResponseField,
    entra_contract::{account_upn, guid, unix_expiry},
};

#[cfg(windows)]
use crate::{
    NativeOperation, NativeStage, TokenAudience,
    entra::{cancelled, classify, provider_for_authority, unix_now},
    native::{ParentWindow, native_error, spawn_with_timeout, start_interactive, wait_for_window},
    request::Shared,
};
#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use windows::{
    Security::Authentication::Web::Core::{WebTokenRequest, WebTokenRequestPromptType},
    core::HSTRING,
};

const PUBLIC_AUTHORITY: &str = "https://login.microsoftonline.com";
const CONSUMER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// Non-secret phase of an explicitly requested WAM account selection.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountSelectionStage {
    /// Waiting for Windows to return its work-account provider.
    Provider,
    /// Waiting for the parented Windows sign-in dialog.
    Interactive,
    /// Validating the returned account, authority and expiry metadata.
    Response,
}

/// Validates a public client GUID and optional work/school tenant/account hints.
///
/// This pure check performs no provider, account, environment or session-store
/// access and does not authorize UI. Missing hints are valid; present hints must
/// be a tenant GUID other than the personal-account tenant, and an account UPN.
/// Empty or malformed hints fail rather than falling back to a saved identity.
/// Hosts can call this before any session-state I/O. [`select_account`] reuses
/// the same validation before provider access.
///
/// # Errors
///
/// Returns [`BrokerError::InvalidConfiguration`] without echoing supplied values
/// when the client or a supplied hint is invalid.
pub fn validate_selection(
    client: &str,
    tenant: Option<&str>,
    account: Option<&str>,
) -> Result<(), BrokerError> {
    if !guid(client)
        || tenant.is_some_and(|value| !guid(value) || value.eq_ignore_ascii_case(CONSUMER_TENANT))
        || account.is_some_and(|value| !account_upn(value))
    {
        Err(BrokerError::InvalidConfiguration)
    } else {
        Ok(())
    }
}

// Neither hints nor response metadata implement Debug: they contain account PII.
struct SelectionPlan {
    client: String,
    tenant: Option<String>,
    account: Option<String>,
}

impl SelectionPlan {
    fn new(
        client: String,
        tenant: Option<String>,
        account: Option<String>,
        interaction: Interaction,
    ) -> Result<Self, BrokerError> {
        validate_selection(&client, tenant.as_deref(), account.as_deref())?;
        if interaction == Interaction::Silent {
            return Err(BrokerError::InteractionRequired);
        }
        Ok(Self {
            client: client.to_ascii_lowercase(),
            tenant: tenant.map(|value| value.to_ascii_lowercase()),
            account,
        })
    }

    fn provider_authority(&self) -> &str {
        self.tenant.as_deref().unwrap_or("organizations")
    }

    fn authority(&self) -> String {
        format!("{PUBLIC_AUTHORITY}/{}", self.provider_authority())
    }

    fn request_properties(&self) -> Vec<(&'static str, String)> {
        let mut properties = vec![
            ("authority", self.authority()),
            ("validateAuthority", "yes".to_owned()),
            ("wam_compat", "2.0".to_owned()),
            ("prompt", "select_account".to_owned()),
        ];
        if let Some(account) = &self.account {
            properties.push(("LoginHint", account.clone()));
        }
        properties
    }

    fn force_authentication(&self) -> bool {
        self.account.is_some()
    }
}

struct SelectionResponse<'a> {
    id: &'a str,
    username: &'a str,
    authority: &'a str,
    tenant_id: Option<&'a str>,
    expires: &'a str,
    has_token: bool,
}

fn single_response(count: u32) -> Result<(), BrokerError> {
    if count == 1 {
        Ok(())
    } else {
        Err(metadata_error(ResponseField::RecordCount))
    }
}

fn metadata_error(field: ResponseField) -> BrokerError {
    BrokerError::ResponseMetadata { field }
}

fn selection_from_response(
    plan: &SelectionPlan,
    response: SelectionResponse<'_>,
    now: i64,
) -> Result<EntraOptions, BrokerError> {
    if response.id.is_empty() {
        return Err(metadata_error(ResponseField::AccountId));
    }
    if !account_upn(response.username) {
        return Err(metadata_error(ResponseField::AccountName));
    }
    if !response.has_token {
        return Err(metadata_error(ResponseField::Token));
    }
    let (_, authority_tenant) = response
        .authority
        .trim_end_matches('/')
        .rsplit_once('/')
        .ok_or_else(|| metadata_error(ResponseField::Authority))?;
    let neutral = plan.tenant.is_none()
        && response
            .authority
            .trim_end_matches('/')
            .eq_ignore_ascii_case(&format!("{PUBLIC_AUTHORITY}/organizations"));
    let tenant = if neutral {
        response
            .tenant_id
            .filter(|tenant| guid(tenant))
            .ok_or_else(|| metadata_error(ResponseField::TenantId))?
    } else {
        authority_tenant
    };
    let options = EntraOptions::new(tenant, &plan.client, response.username)
        .map_err(|_| metadata_error(ResponseField::Authority))?;
    if !neutral {
        options
            .validate_authority(response.authority)
            .map_err(|_| metadata_error(ResponseField::Authority))?;
    }
    if options.tenant().eq_ignore_ascii_case(CONSUMER_TENANT) {
        return Err(metadata_error(ResponseField::TenantId));
    }
    if plan
        .tenant
        .as_deref()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(options.tenant()))
        || plan
            .account
            .as_deref()
            .is_some_and(|expected| !expected.eq_ignore_ascii_case(options.account()))
    {
        return Err(BrokerError::IdentityMismatch);
    }
    unix_expiry(response.expires, now).map_err(|_| metadata_error(ResponseField::Expiry))?;
    Ok(options)
}

/// Asks WAM to select a public-cloud work/school account for an explicit login.
///
/// The client must be a registered public-client GUID. Optional tenant GUID and
/// account UPN hints are validated before provider access and must match the
/// returned identity, ignoring ASCII case. Without a tenant hint the request
/// uses `https://login.microsoftonline.com/organizations`; it never infers a
/// tenant from a UPN domain or silently picks a cached/default Windows account.
/// `Interaction::Silent` fails before provider access. Allowed selection always
/// requests interactive `select_account` UI on the existing owned STA worker.
///
/// A Fabric bootstrap request supplies authoritative account/tenant/expiry
/// metadata. Its token is checked for presence and discarded, never returned or
/// persisted by this API. Success returns only validated, non-secret options,
/// **not** a logged-in session or a native-account pin. The caller must still
/// perform its existing coordinated login for all three audiences. Same-UPN
/// ambiguity during that acquisition remains an error, not a fallback.
///
/// Call this only for an explicitly interactive login with incomplete selectors,
/// never for status, logout, SDK refresh or ordinary commands. When both selectors
/// are supplied, callers should use [`EntraOptions::new`] directly instead.
///
/// Dropping/cancelling the future requests cooperative native cleanup. The
/// operation shares the existing bounded worker pool and 600-second native
/// deadline; callers should impose their own shorter local-wait deadline. WAM
/// can update its secure cache even when selection or subsequent login fails.
///
/// # Errors
///
/// Awaiting reports sanitized configuration, interaction, response, identity,
/// provider, timeout, cancellation or cleanup errors. Missing or non-public-cloud
/// authority metadata is rejected without decoding tokens or guessing a tenant.
/// A tenant-neutral `organizations` response can resolve its tenant only from
/// the same returned Windows account's validated `TenantId` property. A later
/// tenant-bound login still validates all three audiences before session storage.
#[cfg(windows)]
pub fn select_account(
    client: String,
    tenant: Option<String>,
    account: Option<String>,
    interaction: Interaction,
) -> NativeOperation<EntraOptions> {
    select_account_with_progress(client, tenant, account, interaction, |_| {})
}

/// Selects an account while reporting only the current native phase.
///
/// The callback runs on the owned worker thread. It must not block, panic or
/// perform UI work. It receives no account data, provider text or tokens. The
/// authority, validation, cancellation and ownership rules of [`select_account`]
/// are unchanged; progress is not evidence that native work completed.
///
/// # Errors
///
/// Returns the same sanitized failures as [`select_account`].
#[cfg(windows)]
pub fn select_account_with_progress(
    client: String,
    tenant: Option<String>,
    account: Option<String>,
    interaction: Interaction,
    progress: impl Fn(AccountSelectionStage) + Send + 'static,
) -> NativeOperation<EntraOptions> {
    let plan = SelectionPlan::new(client, tenant, account, interaction);
    spawn_with_timeout(Duration::from_secs(600), move |shared| {
        with_selection(plan, shared, |plan, shared| {
            select_interactively(plan, shared, &progress)
        })
    })
}

#[cfg(windows)]
fn with_selection(
    plan: Result<SelectionPlan, BrokerError>,
    shared: &Shared<EntraOptions>,
    select: impl FnOnce(&SelectionPlan, &Shared<EntraOptions>) -> Result<EntraOptions, BrokerError>,
) -> Result<EntraOptions, BrokerError> {
    let plan = plan?;
    cancelled(shared)?;
    let options = select(&plan, shared)?;
    cancelled(shared)?;
    Ok(options)
}

#[cfg(windows)]
fn select_interactively(
    plan: &SelectionPlan,
    shared: &Shared<EntraOptions>,
    progress: &impl Fn(AccountSelectionStage),
) -> Result<EntraOptions, BrokerError> {
    progress(AccountSelectionStage::Provider);
    let provider = provider_for_authority(plan.provider_authority(), shared)?;
    cancelled(shared)?;
    let scope = format!(
        "{} openid profile offline_access",
        TokenAudience::Fabric.scope()
    );
    let request = WebTokenRequest::CreateWithPromptType(
        &provider,
        &HSTRING::from(scope),
        &HSTRING::from(plan.client.as_str()),
        if plan.force_authentication() {
            WebTokenRequestPromptType::ForceAuthentication
        } else {
            WebTokenRequestPromptType::Default
        },
    )
    .map_err(|error| native_error(NativeStage::Request, error))?;
    let properties = request
        .Properties()
        .map_err(|error| native_error(NativeStage::Request, error))?;
    for (key, value) in plan.request_properties() {
        properties
            .Insert(&HSTRING::from(key), &HSTRING::from(value))
            .map_err(|error| native_error(NativeStage::Request, error))?;
    }
    cancelled(shared)?;
    let mut parent = ParentWindow::for_sign_in()?;
    parent.track_cleanup(shared.control());
    progress(AccountSelectionStage::Interactive);
    let result = cancelled(shared)
        .and_then(|()| start_interactive(request, None, &parent))
        .and_then(|operation| wait_for_window(operation, shared, &parent));
    if let Err(error) = parent.close() {
        shared.uncertain_cleanup();
        return Err(error);
    }
    let result = result?;
    progress(AccountSelectionStage::Response);
    classify(&result)?;
    let responses = result
        .ResponseData()
        .map_err(|error| native_error(NativeStage::Response, error))?;
    single_response(
        responses
            .Size()
            .map_err(|error| native_error(NativeStage::Response, error))?,
    )?;
    let response = responses
        .GetAt(0)
        .map_err(|error| native_error(NativeStage::Response, error))?;
    let selected = response
        .WebAccount()
        .map_err(|_| metadata_error(ResponseField::Account))?;
    let id = selected
        .Id()
        .map_err(|_| metadata_error(ResponseField::AccountId))?
        .to_string();
    let username = selected
        .UserName()
        .map_err(|_| metadata_error(ResponseField::AccountName))?
        .to_string();
    let properties = response
        .Properties()
        .map_err(|_| metadata_error(ResponseField::Properties))?;
    let authority = properties
        .Lookup(&HSTRING::from("Authority"))
        .map_err(|_| metadata_error(ResponseField::Authority))?
        .to_string();
    let tenant_id = if plan.tenant.is_none()
        && authority
            .trim_end_matches('/')
            .eq_ignore_ascii_case(&format!("{PUBLIC_AUTHORITY}/organizations"))
    {
        Some(
            selected
                .Properties()
                .and_then(|properties| properties.Lookup(&HSTRING::from("TenantId")))
                .map_err(|_| metadata_error(ResponseField::TenantId))?
                .to_string(),
        )
    } else {
        None
    };
    let expires = properties
        .Lookup(&HSTRING::from("TokenExpiresOn"))
        .map_err(|_| metadata_error(ResponseField::Expiry))?
        .to_string();
    let has_token = !response
        .Token()
        .map_err(|_| metadata_error(ResponseField::Token))?
        .is_empty();
    selection_from_response(
        plan,
        SelectionResponse {
            id: &id,
            username: &username,
            authority: &authority,
            tenant_id: tenant_id.as_deref(),
            expires: &expires,
            has_token,
        },
        unix_now()?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TENANT: &str = "11111111-2222-3333-4444-555555555555";
    const OTHER_TENANT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const CLIENT: &str = "12345678-9abc-def0-1234-56789abcdef0";
    const USER: &str = "user@unrelated.example.test";
    const NOW: i64 = 1_700_000_000;

    fn plan(tenant: Option<&str>, account: Option<&str>) -> SelectionPlan {
        SelectionPlan::new(
            CLIENT.to_owned(),
            tenant.map(str::to_owned),
            account.map(str::to_owned),
            Interaction::Allowed,
        )
        .unwrap()
    }

    fn response(authority: &str) -> SelectionResponse<'_> {
        SelectionResponse {
            id: "synthetic-native-id",
            username: USER,
            authority,
            tenant_id: None,
            expires: "13344477200",
            has_token: true,
        }
    }

    fn assert_response_error(
        plan: &SelectionPlan,
        response: SelectionResponse<'_>,
        expected: BrokerError,
    ) {
        assert_eq!(
            selection_from_response(plan, response, NOW).unwrap_err(),
            expected
        );
    }

    #[test]
    fn bare_selection_uses_organizations_only_as_bootstrap_authority() {
        let plan = plan(None, None);
        assert!(!plan.force_authentication());
        assert_eq!(plan.provider_authority(), "organizations");
        assert_eq!(
            plan.authority(),
            "https://login.microsoftonline.com/organizations"
        );
        assert_eq!(
            plan.request_properties(),
            [
                (
                    "authority",
                    "https://login.microsoftonline.com/organizations".to_owned()
                ),
                ("validateAuthority", "yes".to_owned()),
                ("wam_compat", "2.0".to_owned()),
                ("prompt", "select_account".to_owned()),
            ]
        );
        for tenant in ["common", "organizations"] {
            assert!(EntraOptions::new(tenant, CLIENT, USER).is_err());
            assert_response_error(
                &plan,
                response(&format!("{PUBLIC_AUTHORITY}/{tenant}")),
                metadata_error(if tenant == "organizations" {
                    ResponseField::TenantId
                } else {
                    ResponseField::Authority
                }),
            );
        }
    }

    #[test]
    fn optional_hints_are_validated_without_provider_access() {
        for client in ["", "not-a-guid", "organizations", "../client"] {
            assert_eq!(
                validate_selection(client, None, None),
                Err(BrokerError::InvalidConfiguration)
            );
            assert_eq!(
                SelectionPlan::new(client.into(), None, None, Interaction::Allowed).err(),
                Some(BrokerError::InvalidConfiguration)
            );
        }
        for tenant in [
            "",
            "common",
            "organizations",
            "consumers",
            "https://login.microsoftonline.com/tenant",
            CONSUMER_TENANT,
        ] {
            assert_eq!(
                validate_selection(CLIENT, Some(tenant), None),
                Err(BrokerError::InvalidConfiguration)
            );
            assert_eq!(
                SelectionPlan::new(
                    CLIENT.into(),
                    Some(tenant.into()),
                    None,
                    Interaction::Allowed
                )
                .err(),
                Some(BrokerError::InvalidConfiguration)
            );
        }
        for account in [
            "",
            "no-at-sign",
            "@example.test",
            "user@",
            "user@@example.test",
            "user name@example.test",
            "user\n@example.test",
        ] {
            assert_eq!(
                validate_selection(CLIENT, None, Some(account)),
                Err(BrokerError::InvalidConfiguration)
            );
            assert_eq!(
                SelectionPlan::new(
                    CLIENT.into(),
                    None,
                    Some(account.into()),
                    Interaction::Allowed
                )
                .err(),
                Some(BrokerError::InvalidConfiguration)
            );
        }
    }

    #[test]
    fn public_validation_accepts_missing_hints_but_never_ignores_present_bad_hints() {
        for (tenant, account) in [
            (None, None),
            (Some(TENANT), None),
            (None, Some(USER)),
            (Some(TENANT), Some(USER)),
        ] {
            assert_eq!(validate_selection(CLIENT, tenant, account), Ok(()));
        }
        for (tenant, account) in [
            (Some("invalid-tenant"), None),
            (Some("invalid-tenant"), Some(USER)),
            (None, Some("invalid-account")),
            (Some(TENANT), Some("invalid-account")),
            (Some(""), None),
            (None, Some("")),
        ] {
            assert_eq!(
                validate_selection(CLIENT, tenant, account),
                Err(BrokerError::InvalidConfiguration)
            );
        }
        assert_eq!(
            validate_selection(
                &CLIENT.to_ascii_uppercase(),
                Some(&OTHER_TENANT.to_ascii_uppercase()),
                Some(&USER.to_ascii_uppercase()),
            ),
            Ok(())
        );
    }

    #[test]
    fn silent_selection_fails_even_with_a_potentially_cached_account() {
        for (tenant, account) in [
            (None, None),
            (Some(TENANT), None),
            (None, Some(USER)),
            (Some(TENANT), Some(USER)),
        ] {
            assert_eq!(
                SelectionPlan::new(
                    CLIENT.into(),
                    tenant.map(str::to_owned),
                    account.map(str::to_owned),
                    Interaction::Silent
                )
                .err(),
                Some(BrokerError::InteractionRequired)
            );
        }
        assert_eq!(
            SelectionPlan::new("invalid-client".into(), None, None, Interaction::Silent).err(),
            Some(BrokerError::InvalidConfiguration)
        );
    }

    #[test]
    fn account_only_hint_does_not_infer_a_tenant_from_the_upn_domain() {
        let plan = plan(None, Some(USER));
        assert!(plan.force_authentication());
        assert_eq!(plan.provider_authority(), "organizations");
        assert_eq!(
            plan.request_properties().last(),
            Some(&("LoginHint", USER.to_owned()))
        );
        for tenant in [TENANT, OTHER_TENANT] {
            let options = selection_from_response(
                &plan,
                response(&format!("{PUBLIC_AUTHORITY}/{tenant}")),
                NOW,
            )
            .unwrap();
            assert_eq!(options.tenant(), tenant);
            assert_eq!(options.account(), USER);
            assert_eq!(options.client(), CLIENT);
        }
    }

    #[test]
    fn tenant_only_hint_binds_guest_target_without_adding_a_login_hint() {
        let plan = plan(Some(TENANT), None);
        assert_eq!(plan.provider_authority(), TENANT);
        assert_eq!(
            plan.request_properties()[0],
            ("authority", format!("{PUBLIC_AUTHORITY}/{TENANT}"))
        );
        assert!(
            !plan
                .request_properties()
                .iter()
                .any(|(key, _)| *key == "LoginHint")
        );
        assert!(
            selection_from_response(
                &plan,
                response(&format!("{PUBLIC_AUTHORITY}/{TENANT}")),
                NOW
            )
            .is_ok()
        );
        assert_response_error(
            &plan,
            response(&format!("{PUBLIC_AUTHORITY}/{OTHER_TENANT}")),
            BrokerError::IdentityMismatch,
        );
    }

    #[test]
    fn explicit_account_and_tenant_hints_cannot_be_overridden() {
        let plan = plan(Some(OTHER_TENANT), Some(USER));
        let authority = format!("{PUBLIC_AUTHORITY}/{OTHER_TENANT}");
        let options = selection_from_response(&plan, response(&authority), NOW).unwrap();
        assert_eq!(options.tenant(), OTHER_TENANT);
        let mut wrong_account = response(&authority);
        wrong_account.username = "other@example.test";
        assert_response_error(&plan, wrong_account, BrokerError::IdentityMismatch);
        assert_response_error(
            &plan,
            response(&format!("{PUBLIC_AUTHORITY}/{TENANT}")),
            BrokerError::IdentityMismatch,
        );
        assert_response_error(
            &self::plan(None, Some(USER)),
            SelectionResponse {
                username: "other@example.test",
                ..response(&authority)
            },
            BrokerError::IdentityMismatch,
        );
    }

    #[test]
    fn guid_authority_and_upn_case_and_trailing_slash_follow_existing_validation() {
        let plan = SelectionPlan::new(
            CLIENT.to_ascii_uppercase(),
            Some(OTHER_TENANT.to_ascii_uppercase()),
            Some(USER.to_ascii_uppercase()),
            Interaction::Allowed,
        )
        .unwrap();
        let authority = format!(
            "HTTPS://LOGIN.MICROSOFTONLINE.COM/{}/",
            OTHER_TENANT.to_ascii_uppercase()
        );
        let options = selection_from_response(&plan, response(&authority), NOW).unwrap();
        assert_eq!(options.tenant(), OTHER_TENANT);
        assert_eq!(options.client(), CLIENT);
        assert_eq!(options.account(), USER);
    }

    #[test]
    fn authority_must_be_an_exact_public_https_guid_authority() {
        let plan = plan(None, None);
        for authority in [
            "".to_owned(),
            format!("http://login.microsoftonline.com/{TENANT}"),
            format!("https://login.microsoftonline.com.evil.test/{TENANT}"),
            format!("https://evil.test/login.microsoftonline.com/{TENANT}"),
            format!("https://login.microsoftonline.com@evil.test/{TENANT}"),
            format!("https://evil.test@login.microsoftonline.com/{TENANT}"),
            format!("https://login.microsoftonline.com:443/{TENANT}"),
            format!("https://login.microsoftonline.com/{TENANT}?tenant=other"),
            format!("https://login.microsoftonline.com/{TENANT}#fragment"),
            format!("https://login.microsoftonline.com/path/{TENANT}"),
            format!("https://login.microsoftonline.com/{TENANT}/oauth2"),
            format!("https://login.microsoftonline.com/../{TENANT}"),
            format!("https://login.microsoftonline.com/%2f{TENANT}"),
            format!("https://login.microsoftonline.com/{TENANT} "),
            format!(" https://login.microsoftonline.com/{TENANT}"),
            format!("https://login.microsoftonline.us/{TENANT}"),
            format!("https://login.chinacloudapi.cn/{TENANT}"),
            format!("https://login.microsoftonline.com/{CONSUMER_TENANT}"),
            "https://login.microsoftonline.com/consumers".to_owned(),
            "https://login.microsoftonline.com/not-a-guid".to_owned(),
            "https://login.microsoftonline.com/11111111-2222-3333-4444-55555555555g".to_owned(),
        ] {
            assert_response_error(
                &plan,
                response(&authority),
                metadata_error(if authority.ends_with(CONSUMER_TENANT) {
                    ResponseField::TenantId
                } else {
                    ResponseField::Authority
                }),
            );
        }
    }

    #[test]
    fn exactly_one_native_response_is_required() {
        assert_eq!(single_response(1), Ok(()));
        for count in [0, 2, 3, u32::MAX] {
            assert_eq!(
                single_response(count),
                Err(metadata_error(ResponseField::RecordCount))
            );
        }
    }

    #[test]
    fn identity_token_and_expiry_metadata_are_required_without_token_decoding() {
        let plan = plan(None, None);
        let authority = format!("{PUBLIC_AUTHORITY}/{TENANT}");
        for (invalid, field) in [
            (
                SelectionResponse {
                    id: "",
                    ..response(&authority)
                },
                ResponseField::AccountId,
            ),
            (
                SelectionResponse {
                    username: "",
                    ..response(&authority)
                },
                ResponseField::AccountName,
            ),
            (
                SelectionResponse {
                    username: "not-a-upn",
                    ..response(&authority)
                },
                ResponseField::AccountName,
            ),
            (
                SelectionResponse {
                    has_token: false,
                    ..response(&authority)
                },
                ResponseField::Token,
            ),
        ] {
            assert_response_error(&plan, invalid, metadata_error(field));
        }
        for expires in [
            "",
            "0",
            "-1",
            "1700003600",
            "133444772000000000",
            "18446744073709551615",
            "not-an-expiry",
            "13344473600",
        ] {
            assert_response_error(
                &plan,
                SelectionResponse {
                    expires,
                    ..response(&authority)
                },
                metadata_error(ResponseField::Expiry),
            );
        }
        assert_response_error(
            &plan,
            response("https://login.microsoftonline.com/organizations"),
            metadata_error(ResponseField::TenantId),
        );
    }

    #[test]
    fn tenant_neutral_response_requires_the_same_broker_accounts_explicit_tenant_metadata() {
        let plan = plan(None, None);
        for authority in [
            "https://login.microsoftonline.com/organizations",
            "HTTPS://LOGIN.MICROSOFTONLINE.COM/ORGANIZATIONS/",
        ] {
            let selected = selection_from_response(
                &plan,
                SelectionResponse {
                    tenant_id: Some(TENANT),
                    ..response(authority)
                },
                NOW,
            )
            .unwrap();
            assert_eq!(selected.tenant(), TENANT);
            assert_eq!(selected.account(), USER);
            assert_eq!(selected.client(), CLIENT);
        }
        for tenant_id in [
            None,
            Some(""),
            Some("organizations"),
            Some("common"),
            Some("https://evil.test"),
            Some(CONSUMER_TENANT),
            Some("invalid-tenant"),
        ] {
            assert_response_error(
                &plan,
                SelectionResponse {
                    tenant_id,
                    ..response("https://login.microsoftonline.com/organizations")
                },
                metadata_error(ResponseField::TenantId),
            );
        }
    }

    #[test]
    fn tenant_metadata_cannot_override_the_resource_tenant_or_authorize_an_invalid_authority() {
        let plan = plan(Some(TENANT), Some(USER));
        let selected = selection_from_response(
            &plan,
            SelectionResponse {
                tenant_id: Some(OTHER_TENANT),
                ..response(&format!("{PUBLIC_AUTHORITY}/{TENANT}"))
            },
            NOW,
        )
        .unwrap();
        assert_eq!(selected.tenant(), TENANT);
        assert_response_error(
            &plan,
            SelectionResponse {
                tenant_id: Some(TENANT),
                ..response("https://login.microsoftonline.com/organizations")
            },
            metadata_error(ResponseField::Authority),
        );
        let neutral_plan = self::plan(None, None);
        for authority in [
            "https://evil.test/organizations",
            "https://login.microsoftonline.com.evil.test/organizations",
            "http://login.microsoftonline.com/organizations",
            "https://login.microsoftonline.com/common",
            "https://login.microsoftonline.com/organizations?tenant=other",
            "https://login.microsoftonline.com/organizations/oauth2",
        ] {
            assert_response_error(
                &neutral_plan,
                SelectionResponse {
                    tenant_id: Some(TENANT),
                    ..response(authority)
                },
                metadata_error(ResponseField::Authority),
            );
        }
    }

    #[test]
    fn tenant_neutral_selection_preserves_all_other_identity_token_and_expiry_guards() {
        let plan = plan(None, Some(USER));
        let make_response = || SelectionResponse {
            tenant_id: Some(TENANT),
            ..response("https://login.microsoftonline.com/organizations")
        };
        assert_response_error(
            &plan,
            SelectionResponse {
                username: "different@example.test",
                ..make_response()
            },
            BrokerError::IdentityMismatch,
        );
        assert_response_error(
            &plan,
            SelectionResponse {
                has_token: false,
                ..make_response()
            },
            metadata_error(ResponseField::Token),
        );
        assert_response_error(
            &plan,
            SelectionResponse {
                expires: "0",
                ..make_response()
            },
            metadata_error(ResponseField::Expiry),
        );
        assert_response_error(
            &plan,
            SelectionResponse {
                id: "",
                ..make_response()
            },
            metadata_error(ResponseField::AccountId),
        );
    }

    #[test]
    fn metadata_diagnostics_only_contain_fixed_field_labels_not_provider_values() {
        for field in [
            ResponseField::RecordCount,
            ResponseField::Account,
            ResponseField::AccountId,
            ResponseField::AccountName,
            ResponseField::Properties,
            ResponseField::Authority,
            ResponseField::TenantId,
            ResponseField::Expiry,
            ResponseField::Token,
        ] {
            let error = metadata_error(field);
            assert!(error.to_string().contains(&field.to_string()));
            assert!(!error.to_string().contains(USER));
            assert!(!error.to_string().contains(TENANT));
        }
    }

    #[test]
    fn errors_do_not_echo_the_native_identity_or_user_supplied_hints() {
        let plan = plan(None, Some("distinctive-private-user@example.test"));
        let error = selection_from_response(
            &plan,
            response(&format!("{PUBLIC_AUTHORITY}/{TENANT}")),
            NOW,
        )
        .unwrap_err();
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains("distinctive-private-user"));
            assert!(!text.contains(USER));
            assert!(!text.contains("synthetic-native-id"));
            assert!(!text.contains(TENANT));
        }
    }

    #[cfg(windows)]
    #[test]
    fn cancelled_selection_never_enters_the_provider_or_ui() {
        let shared = Shared::new();
        shared.cancel();
        let result = with_selection(Ok(plan(None, None)), &shared, |_, _| {
            panic!("cancelled selection must not start provider work")
        });
        assert_eq!(result.unwrap_err(), BrokerError::Cancelled);
    }

    #[cfg(windows)]
    #[test]
    fn silent_or_invalid_selection_never_enters_the_provider_or_ui() {
        for plan in [
            SelectionPlan::new(CLIENT.into(), None, None, Interaction::Silent),
            SelectionPlan::new("invalid-client".into(), None, None, Interaction::Allowed),
        ] {
            let expected = plan.err().unwrap();
            let shared = Shared::new();
            let result = with_selection(Err(expected), &shared, |_, _| {
                panic!("rejected selection must not start provider work")
            });
            assert_eq!(result.unwrap_err(), expected);
        }
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_after_bootstrap_suppresses_selected_options() {
        let shared = Shared::new();
        let result = with_selection(Ok(plan(None, None)), &shared, |_, shared| {
            shared.cancel();
            EntraOptions::new(TENANT, CLIENT, USER)
        });
        assert_eq!(result.unwrap_err(), BrokerError::Cancelled);
    }
}
