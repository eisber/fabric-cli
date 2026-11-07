// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Non-secret selection of an explicit protected device-code session.

use fabric_cli::{
    auth_commands::Failure,
    parser::{AuthAction, AuthCommand},
};
use fabric_identity::interactive::{InteractiveError, InteractiveOptions, OsSessionStore};
use std::sync::Arc;

pub(crate) struct Selection {
    pub(crate) tenant: String,
    pub(crate) account: String,
    pub(crate) client: String,
    pub(crate) binding: String,
    pub(crate) options: InteractiveOptions,
    pub(crate) store: Arc<OsSessionStore>,
}

fn environment(name: &str) -> Result<Option<String>, InteractiveError> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        _ => Err(InteractiveError::Configuration),
    }
}

pub(crate) fn store() -> Result<Arc<OsSessionStore>, InteractiveError> {
    let root = crate::runtime_config::available_root()
        .map_err(|_| InteractiveError::Configuration)?
        .ok_or(InteractiveError::Configuration)?;
    let store = OsSessionStore::new(root)?;
    #[cfg(target_os = "macos")]
    let store = store.with_macos_helper(protected_helper(environment)?)?;
    Ok(Arc::new(store))
}

#[cfg(target_os = "macos")]
fn protected_helper(
    mut read: impl FnMut(&str) -> Result<Option<String>, InteractiveError>,
) -> Result<fabric_identity::interactive::macos::MacProtectedStoreHelper, InteractiveError> {
    use fabric_identity::interactive::macos::MacProtectedStoreHelper;
    let path = read("FAB_RUST_PROTECTED_STORE_HELPER")?.ok_or(InteractiveError::Configuration)?;
    let hash =
        read("FAB_RUST_PROTECTED_STORE_HELPER_SHA256")?.ok_or(InteractiveError::Configuration)?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(InteractiveError::Configuration);
    }
    let mut sha256 = [0; 32];
    for (index, pair) in hash.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let pair = std::str::from_utf8(pair).map_err(|_| InteractiveError::Configuration)?;
        sha256[index] =
            u8::from_str_radix(pair, 16).map_err(|_| InteractiveError::Configuration)?;
    }
    MacProtectedStoreHelper::new(path.into(), sha256, std::time::Duration::from_secs(30))
}

pub(crate) fn options(
    command: &AuthCommand,
    store: &OsSessionStore,
) -> Result<InteractiveOptions, InteractiveError> {
    let saved = store.selection()?;
    let saved = saved.as_ref().filter(|saved| saved.session_id.is_some());
    let tenant = command
        .tenant
        .clone()
        .map(|value| Ok(Some(value)))
        .unwrap_or_else(|| environment("FAB_RUST_TENANT"))?;
    let client = if command.client_id_explicit {
        Some(command.client_id.clone())
    } else {
        environment("FAB_RUST_CLIENT_ID")?
    };
    let account = command
        .account
        .clone()
        .map(|value| Ok(Some(value)))
        .unwrap_or_else(|| environment("FAB_RUST_ACCOUNT"))?;
    // Login deliberately requires explicit directory/application selection. No
    // default WAM registration or guessed tenant is imported into device code.
    let (tenant, client) = if command.action == AuthAction::Login {
        (tenant, client)
    } else {
        (
            tenant.or_else(|| {
                saved
                    .and_then(|saved| saved.identity.as_ref())
                    .map(|identity| identity.tenant.clone())
            }),
            client.or_else(|| saved.and_then(|saved| saved.client.clone())),
        )
    };
    InteractiveOptions::new(
        tenant.ok_or(InteractiveError::Configuration)?,
        client.ok_or(InteractiveError::Configuration)?,
        account,
    )
}

pub(crate) fn selected() -> Result<Selection, Failure> {
    let selected = (|| {
        let store = store()?;
        let saved = store.selection()?.ok_or(InteractiveError::NotConfigured)?;
        let identity = saved.identity.ok_or(InteractiveError::NotConfigured)?;
        let client = saved.client.ok_or(InteractiveError::NotConfigured)?;
        let session_id = saved.session_id.ok_or(InteractiveError::NotConfigured)?;
        let tenant = environment("FAB_RUST_TENANT")?.unwrap_or_else(|| identity.tenant.clone());
        let selected_client = environment("FAB_RUST_CLIENT_ID")?.unwrap_or_else(|| client.clone());
        let account_hint = environment("FAB_RUST_ACCOUNT")?;
        let options = InteractiveOptions::new(tenant, selected_client, account_hint)?;
        if options.tenant() != identity.tenant || options.client() != client {
            return Err(InteractiveError::IdentityMismatch);
        }

        Ok(Selection {
            tenant: identity.tenant,
            account: identity.account,
            client,
            binding: format!("device-code:{}:{session_id}", identity.object_id),
            options,
            store,
        })
    })();
    selected.map_err(super::interactive::failure)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #![deny(unnameable_test_items)]
    use super::*;

    #[test]
    fn protected_helper_requires_independent_explicit_path_and_digest_without_discovery() {
        let path = std::env::current_dir()
            .unwrap()
            .join("not-executed-protected-helper");
        let path = path.to_str().unwrap();
        let digest = "04".repeat(32);
        for (helper, hash) in [
            (None, None),
            (Some(path), None),
            (None, Some(digest.as_str())),
            (Some(path), Some("invalid-secret-canary")),
            (Some("relative-helper"), Some(digest.as_str())),
        ] {
            let result = protected_helper(|name| {
                Ok(match name {
                    "FAB_RUST_PROTECTED_STORE_HELPER" => helper.map(str::to_owned),
                    "FAB_RUST_PROTECTED_STORE_HELPER_SHA256" => hash.map(str::to_owned),
                    _ => panic!("unexpected discovery"),
                })
            });
            let error = result.unwrap_err();
            assert_eq!(error, InteractiveError::Configuration);
            assert!(!format!("{error} {error:?}").contains("canary"));
        }
        let helper = protected_helper(|name| {
            Ok(Some(match name {
                "FAB_RUST_PROTECTED_STORE_HELPER" => path.into(),
                "FAB_RUST_PROTECTED_STORE_HELPER_SHA256" => digest.clone(),
                _ => panic!("unexpected discovery"),
            }))
        })
        .unwrap();
        assert!(!format!("{helper:?}").contains("not-executed"));
    }
}
