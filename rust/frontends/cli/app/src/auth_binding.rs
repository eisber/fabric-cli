// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Non-secret broker selection, separate from WAM-owned tokens and local settings.

use fabric_cli::auth_commands::Failure;
use fabric_identity::LoginOptions;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fabric_cli::parser::DEFAULT_BROKER_CLIENT_ID as DEFAULT_CLIENT;
const FILE_NAME: &str = "broker-session.json";
const MAX_BYTES: u64 = 4096;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn invalid() -> Failure {
    (
        "invalid_configuration",
        "The saved Windows broker selection is invalid. Remove broker-session.json from the configured Fabric CLI directory and run auth login again.".into(),
    )
}

fn storage(error: io::Error) -> Failure {
    (
        "auth_state_failed",
        format!(
            "Cannot access the saved Windows broker selection ({:?}). Check the Fabric CLI directory permissions or set FAB_RUST_CONFIG_ROOT to a writable absolute directory. No tokens are saved in this file.",
            error.kind()
        ),
    )
}

#[cfg(any(feature = "native-host", test))]
fn absent() -> Failure {
    (
        "NotConfigured",
        "No Windows broker account is selected. Run fab-rs auth login, or set FAB_RUST_TENANT, FAB_RUST_ACCOUNT and FAB_RUST_CLIENT_ID explicitly.".into(),
    )
}

fn reject_links(path: &Path) -> Result<(), Failure> {
    use std::os::windows::fs::MetadataExt;
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_attributes() & 0x400 != 0 => return Err(invalid()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
    }
    Ok(())
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    schema: u8,
    generation: u64,
    pub(crate) tenant: String,
    pub(crate) account: String,
    pub(crate) client: String,
}

impl Binding {
    pub(crate) fn from_options(options: &LoginOptions) -> Self {
        Self {
            schema: 1,
            generation: 0,
            tenant: options.tenant().into(),
            account: options.account().into(),
            client: options.client().into(),
        }
    }

    fn validate(&self) -> Result<(), Failure> {
        if self.schema != 1
            || self.generation == 0
            || LoginOptions::new(&self.tenant, &self.client, &self.account).is_err()
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub(crate) fn matches(&self, options: &LoginOptions) -> bool {
        self.tenant.eq_ignore_ascii_case(options.tenant())
            && self.account.eq_ignore_ascii_case(options.account())
            && self.client.eq_ignore_ascii_case(options.client())
    }
}

pub(crate) struct Store {
    root: PathBuf,
}

impl Store {
    pub(crate) fn available() -> Result<Option<Self>, Failure> {
        crate::runtime_config::available_root()
            .map_err(|_| (
                "invalid_configuration",
                "The Windows broker selection directory must be absolute. Set FAB_RUST_CONFIG_ROOT or LOCALAPPDATA to an absolute directory.".into(),
            ))?
            .map(Self::new)
            .transpose()
    }

    pub(crate) fn new(root: PathBuf) -> Result<Self, Failure> {
        if !root.is_absolute() {
            return Err((
                "invalid_configuration",
                "The Windows broker selection directory must be absolute. Set FAB_RUST_CONFIG_ROOT or LOCALAPPDATA to an absolute directory.".into(),
            ));
        }
        reject_links(&root)?;
        Ok(Self { root })
    }

    pub(crate) fn read(&self) -> Result<Option<Binding>, Failure> {
        let path = self.root.join(FILE_NAME);
        reject_links(&path)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage(error)),
        };
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        File::open(&path)
            .map_err(storage)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(storage)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        let binding: Binding = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        binding.validate()?;
        Ok(Some(binding))
    }

    pub(crate) fn prepare(&self) -> Result<(), Failure> {
        let _lock = self.lock()?;
        self.read()?;
        Ok(())
    }

    fn lock(&self) -> Result<File, Failure> {
        reject_links(&self.root)?;
        fs::create_dir_all(&self.root).map_err(storage)?;
        let path = self.root.join(".broker-session.lock");
        reject_links(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(storage)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => (
                "authentication_busy",
                "Another process is updating the saved broker selection. Retry after it completes."
                    .into(),
            ),
            std::fs::TryLockError::Error(error) => storage(error),
        })?;
        Ok(file)
    }

    pub(crate) fn save(&self, options: &LoginOptions) -> Result<(), Failure> {
        let mut lock = self.lock()?;
        let previous = self.read()?;
        let mut counter = Vec::new();
        Read::by_ref(&mut lock)
            .take(9)
            .read_to_end(&mut counter)
            .map_err(storage)?;
        let counter = match counter.as_slice() {
            [] => 0,
            bytes if bytes.len() == 8 => {
                u64::from_le_bytes(bytes.try_into().map_err(|_| invalid())?)
            }
            _ => return Err(invalid()),
        };
        let generation = counter
            .max(previous.as_ref().map_or(0, |binding| binding.generation))
            .checked_add(1)
            .ok_or_else(invalid)?;
        lock.seek(SeekFrom::Start(0)).map_err(storage)?;
        lock.write_all(&generation.to_le_bytes()).map_err(storage)?;
        lock.sync_all().map_err(storage)?;
        let binding = Binding {
            generation,
            ..Binding::from_options(options)
        };
        binding.validate()?;
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary = self.root.join(format!(
            ".broker-session-{}-{sequence}.next",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(storage)?;
        let result = (|| {
            serde_json::to_writer(&mut file, &binding).map_err(|_| invalid())?;
            file.write_all(b"\n").map_err(storage)?;
            file.sync_all().map_err(storage)?;
            drop(file);
            fs::rename(&temporary, self.root.join(FILE_NAME)).map_err(storage)
        })();
        if result.is_err()
            && let Err(error) = fs::remove_file(&temporary)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(storage(error));
        }
        result
    }

    pub(crate) fn clear_if_unchanged(&self, previous: Option<&Binding>) -> Result<(), Failure> {
        let Some(previous) = previous else {
            return Ok(());
        };
        let _lock = self.lock()?;
        if self.read()?.as_ref() == Some(previous) {
            fs::remove_file(self.root.join(FILE_NAME)).map_err(storage)?;
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct Hints {
    pub(crate) tenant: Option<String>,
    pub(crate) account: Option<String>,
    pub(crate) client: Option<String>,
}

fn environment(name: &str) -> Result<Option<String>, Failure> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        _ => Err((
            "invalid_configuration",
            "Invalid Windows broker selector. Check FAB_RUST_TENANT, FAB_RUST_ACCOUNT and FAB_RUST_CLIENT_ID.".into(),
        )),
    }
}

impl Hints {
    pub(crate) fn capture(
        tenant: Option<&str>,
        account: Option<&str>,
        client: Option<&str>,
    ) -> Result<Self, Failure> {
        let explicit_or_environment = |explicit: Option<&str>, name| {
            explicit.map_or_else(|| environment(name), |value| Ok(Some(value.to_owned())))
        };
        let hints = Self {
            tenant: explicit_or_environment(tenant, "FAB_RUST_TENANT")?,
            account: explicit_or_environment(account, "FAB_RUST_ACCOUNT")?,
            client: explicit_or_environment(client, "FAB_RUST_CLIENT_ID")?,
        };
        fabric_identity::windows_broker::validate_selection(
            hints.client.as_deref().unwrap_or(DEFAULT_CLIENT),
            hints.tenant.as_deref(),
            hints.account.as_deref(),
        )
        .map_err(broker_error)?;
        Ok(hints)
    }

    pub(crate) fn resolve(mut self, saved: Option<&Binding>) -> Self {
        let compatible = saved.filter(|binding| {
            self.tenant
                .as_ref()
                .is_none_or(|value| value.eq_ignore_ascii_case(&binding.tenant))
                && self
                    .account
                    .as_ref()
                    .is_none_or(|value| value.eq_ignore_ascii_case(&binding.account))
                && self
                    .client
                    .as_ref()
                    .is_none_or(|value| value.eq_ignore_ascii_case(&binding.client))
        });
        if let Some(saved) = compatible {
            self.tenant.get_or_insert_with(|| saved.tenant.clone());
            self.account.get_or_insert_with(|| saved.account.clone());
            self.client.get_or_insert_with(|| saved.client.clone());
        }
        self.client.get_or_insert_with(|| DEFAULT_CLIENT.into());
        self
    }

    pub(crate) fn complete(&self) -> Result<Option<LoginOptions>, Failure> {
        let (Some(tenant), Some(account), Some(client)) =
            (&self.tenant, &self.account, &self.client)
        else {
            return Ok(None);
        };
        LoginOptions::new(tenant, client, account)
            .map(Some)
            .map_err(broker_error)
    }
}

#[cfg(feature = "native-host")]
pub(crate) fn selected() -> Result<LoginOptions, Failure> {
    let hints = Hints::capture(None, None, None)?;
    selected_from(hints, || {
        Store::available()?
            .as_ref()
            .map(Store::read)
            .transpose()
            .map(Option::flatten)
    })
}

#[cfg(any(feature = "native-host", test))]
fn selected_from(
    hints: Hints,
    read: impl FnOnce() -> Result<Option<Binding>, Failure>,
) -> Result<LoginOptions, Failure> {
    if hints.tenant.is_some() && hints.account.is_some() && hints.client.is_some() {
        return hints.complete()?.ok_or_else(absent);
    }
    let saved = read()?;
    hints.resolve(saved.as_ref()).complete()?.ok_or_else(absent)
}

pub(crate) fn broker_error(
    error: fabric_identity::windows_broker::BrokerError,
) -> (&'static str, String) {
    use fabric_identity::windows_broker::BrokerError;
    let code = match error {
        BrokerError::InvalidConfiguration => "invalid_configuration",
        BrokerError::InteractionRequired => "interaction_required",
        BrokerError::Cancelled => "cancelled",
        BrokerError::IdentityMismatch | BrokerError::AmbiguousAccount => "identity_mismatch",
        BrokerError::InvalidResponse | BrokerError::ResponseMetadata { .. } => {
            "invalid_broker_response"
        }
        BrokerError::ProviderUnavailable => "broker_unavailable",
        BrokerError::InteractiveWindowUnavailable => "interactive_window_unavailable",
        BrokerError::CompletionUnconfirmed { .. } => "completion_unconfirmed",
        BrokerError::IdentityCapacityExhausted { .. } => "identity_capacity_exhausted",
        BrokerError::CapacityExhausted { .. } => "broker_capacity_exhausted",
        BrokerError::OperationInProgress => "authentication_busy",
        BrokerError::SessionInvalidated => "session_invalidated",
        BrokerError::CleanupPending { .. } => "cleanup_pending",
        BrokerError::TimedOut => "timed_out",
        BrokerError::CoordinationFailed => "coordination_failed",
        _ => "authentication_failed",
    };
    (code, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    const TENANT: &str = "11111111-2222-3333-4444-555555555555";
    const ACCOUNT: &str = "selected@example.test";

    fn options() -> LoginOptions {
        LoginOptions::new(TENANT, DEFAULT_CLIENT, ACCOUNT).unwrap()
    }

    fn store() -> Store {
        let parent = std::env::var_os("FAB_IDENTITY_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = parent.join(format!(
            "broker-selection-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        Store::new(root).unwrap()
    }

    fn cleanup(store: Store) {
        fs::remove_dir_all(store.root).unwrap();
    }

    #[test]
    fn saved_binding_has_only_valid_non_secret_selection_and_atomic_updates() {
        let store = store();
        assert!(store.read().unwrap().is_none());
        store.prepare().unwrap();
        assert!(store.read().unwrap().is_none());
        store.save(&options()).unwrap();
        let first = store.read().unwrap().unwrap();
        assert!(first.matches(&options()));
        store.save(&options()).unwrap();
        let second = store.read().unwrap().unwrap();
        assert!(second.generation > first.generation);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(store.root.join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(
            json.as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["schema", "generation", "tenant", "account", "client"]
        );
        cleanup(store);
    }

    #[test]
    fn logout_cannot_clear_a_newer_login_even_for_the_same_account() {
        let store = store();
        store.save(&options()).unwrap();
        let previous = store.read().unwrap();
        store.save(&options()).unwrap();
        store.clear_if_unchanged(previous.as_ref()).unwrap();
        assert!(store.read().unwrap().is_some());
        let current = store.read().unwrap();
        store.clear_if_unchanged(current.as_ref()).unwrap();
        assert!(store.read().unwrap().is_none());
        store.save(&options()).unwrap();
        assert!(store.read().unwrap().unwrap().generation > current.as_ref().unwrap().generation);
        store.clear_if_unchanged(current.as_ref()).unwrap();
        assert!(store.read().unwrap().is_some());
        cleanup(store);
    }

    #[test]
    fn invalid_or_secret_shaped_state_is_not_treated_as_an_empty_session() {
        let store = store();
        store.prepare().unwrap();
        for bytes in [
            b"broken".as_slice(),
            br#"{"schema":1,"generation":1,"tenant":"bad","account":"bad","client":"bad"}"#,
            br#"{"schema":1,"generation":1,"tenant":"bad","account":"bad","client":"bad","token":"canary"}"#,
        ] {
            fs::write(store.root.join(FILE_NAME), bytes).unwrap();
            let error = store.read().err().unwrap();
            assert_eq!(error.0, "invalid_configuration");
            assert!(!error.1.contains("canary"));
        }
        fs::write(
            store.root.join(FILE_NAME),
            vec![b' '; MAX_BYTES as usize + 1],
        )
        .unwrap();
        assert!(store.read().is_err());
        cleanup(store);
    }

    #[test]
    fn busy_state_fails_without_overwriting_another_process_update() {
        let store = store();
        let held = store.lock().unwrap();
        assert_eq!(store.save(&options()).unwrap_err().0, "authentication_busy");
        assert!(store.read().unwrap().is_none());
        drop(held);
        store.save(&options()).unwrap();
        cleanup(store);
    }

    #[test]
    fn matching_hints_reuse_the_binding_but_conflicting_partial_hints_never_mix_accounts() {
        let saved = Binding::from_options(&options());
        let resolved = Hints::default()
            .resolve(Some(&saved))
            .complete()
            .unwrap()
            .unwrap();
        assert!(saved.matches(&resolved));
        let partial = Hints {
            tenant: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
            ..Hints::default()
        };
        assert!(partial.resolve(Some(&saved)).complete().unwrap().is_none());
        let other_client = Hints {
            client: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
            ..Hints::default()
        };
        assert!(
            other_client
                .resolve(Some(&saved))
                .complete()
                .unwrap()
                .is_none()
        );
        let matching = Hints {
            account: Some(ACCOUNT.to_ascii_uppercase()),
            ..Hints::default()
        };
        assert!(saved.matches(&matching.resolve(Some(&saved)).complete().unwrap().unwrap()));
    }

    #[test]
    fn remote_selection_keeps_a_compatible_saved_custom_client_before_applying_defaults() {
        let custom =
            LoginOptions::new(TENANT, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", ACCOUNT).unwrap();
        let hints = Hints {
            tenant: Some(TENANT.into()),
            account: Some(ACCOUNT.into()),
            client: None,
        };
        let selected =
            selected_from(hints.clone(), || Ok(Some(Binding::from_options(&custom)))).unwrap();
        assert_eq!(selected.client(), custom.client());
        let default = selected_from(hints.clone(), || Ok(None)).unwrap();
        assert_eq!(default.client(), DEFAULT_CLIENT);
        let explicit = Hints {
            client: Some(DEFAULT_CLIENT.into()),
            ..hints
        };
        let selected = selected_from(explicit, || {
            panic!("complete explicit selectors must not read saved state")
        })
        .unwrap();
        assert_eq!(selected.client(), DEFAULT_CLIENT);
    }
}
