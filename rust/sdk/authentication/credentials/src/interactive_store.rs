// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{InteractiveError, RefreshSession, SessionIdentity, SessionSnapshot, SessionStore};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(any(windows, target_os = "linux", test))]
const SERVICE: &str = "Microsoft.Fabric.Cli.Rust.DeviceCode.v1";
const SELECTOR: &str = "device-code-session.json";
const LOCK: &str = ".device-code-session.lock";

#[cfg(any(target_os = "linux", test))]
#[path = "interactive_secret_service.rs"]
mod no_ui;

/// Non-secret session selection. No access, refresh or device credentials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSelection {
    /// Schema discriminator; unknown schemas fail closed.
    pub schema: u8,
    /// Provider discriminator, never inferred from secret presence.
    pub provider: String,
    /// Monotonic revision preserved as a logout tombstone.
    pub revision: u64,
    /// OS credential locator, isolated by the explicit configuration root.
    pub locator: String,
    /// Present only for an active login generation.
    pub session_id: Option<u64>,
    /// Selected app GUID, not a credential.
    pub client: Option<String>,
    /// Non-secret endpoint-provenance identity.
    pub identity: Option<SessionIdentity>,
}

/// Explicit OS-protected refresh persistence with a non-secret selector file.
/// File locks and monotonically increasing tombstones reject stale writers
/// across cooperating CLI processes. This is not a defense against local account
/// compromise, malicious selector rollback, or server-side token revocation.
#[derive(Clone)]
pub struct OsSessionStore {
    root: PathBuf,
    locator: String,
    #[cfg(target_os = "macos")]
    helper: Option<super::macos::MacProtectedStoreHelper>,
    #[cfg(target_os = "macos")]
    operation: Option<MacOperation>,
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
pub(super) struct MacOperation {
    pub(super) deadline: std::time::Instant,
    pub(super) cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl OsSessionStore {
    /// Selects one absolute caller-owned config directory without accessing stores.
    ///
    /// # Errors
    /// Unsupported targets and relative paths fail without fallback. On macOS,
    /// construction is inert; operations reject linked paths before opening files.
    /// secret access additionally requires [`Self::with_macos_helper`].
    pub fn new(root: PathBuf) -> Result<Self, InteractiveError> {
        if !cfg!(any(windows, target_os = "macos", target_os = "linux")) {
            return Err(InteractiveError::Unsupported);
        }
        if !root.is_absolute() {
            return Err(InteractiveError::Configuration);
        }
        #[cfg(not(target_os = "macos"))]
        reject_links(&root)?;
        let bytes = root.as_os_str().as_encoded_bytes();
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
        let locator = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            root,
            locator,
            #[cfg(target_os = "macos")]
            helper: None,
            #[cfg(target_os = "macos")]
            operation: None,
        })
    }

    /// Captures a trusted one-shot macOS helper without filesystem/native access.
    ///
    /// Subsequent protected operations require this explicit configuration.
    /// They verify the installation independently on each execution and never
    /// fall back to another helper, provider or plaintext storage. Secret data
    /// travels only through bounded child stdin/stdout, not argv or environment.
    ///
    /// Native/protocol/trust/capacity/deadline/cleanup failures are reported as
    /// [`InteractiveError::Store`]; a missing helper is
    /// [`InteractiveError::Configuration`]. Dropping an operation signals
    /// cancellation while its blocking owner retains the session lock through
    /// bounded child cleanup. Unconfirmed cleanup quarantines capacity; failure
    /// does not assert native rollback. Logout still publishes its durable
    /// tombstone before attempting deletion, and only native item-not-found is
    /// treated as an idempotent deletion.
    ///
    /// # Errors
    /// Returns `Unsupported` off macOS, never another platform's store fallback.
    pub fn with_macos_helper(
        self,
        helper: super::macos::MacProtectedStoreHelper,
    ) -> Result<Self, InteractiveError> {
        #[cfg(target_os = "macos")]
        {
            let mut store = self;
            store.helper = Some(helper);
            Ok(store)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = helper;
            Err(InteractiveError::Unsupported)
        }
    }

    /// Reads only the safe selection; does not access or unlock an OS store.
    ///
    /// # Errors
    /// Invalid metadata and unavailable directories are explicit failures.
    pub fn selection(&self) -> Result<Option<SavedSelection>, InteractiveError> {
        reject_links(&self.root)?;
        let path = self.root.join(SELECTOR);
        reject_links(&path)?;
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(InteractiveError::Store),
        };
        if !file
            .metadata()
            .map_err(|_| InteractiveError::Store)?
            .is_file()
        {
            return Err(InteractiveError::Store);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(8193)
            .read_to_end(&mut bytes)
            .map_err(|_| InteractiveError::Store)?;
        if bytes.len() > 8192 {
            return Err(InteractiveError::Store);
        }
        let value = super::json::parse(&bytes).map_err(|_| InteractiveError::Store)?;
        let saved: SavedSelection =
            serde_json::from_value(value).map_err(|_| InteractiveError::Store)?;
        if saved.schema != 1
            || saved.provider != "device-code"
            || saved.locator != self.locator
            || saved.revision == 0
        {
            return Err(InteractiveError::Store);
        }
        match (&saved.identity, &saved.client, saved.session_id) {
            (Some(identity), Some(client), Some(id))
                if super::is_guid(client) && id > 0 && id <= saved.revision =>
            {
                identity.validate()?
            }
            (None, None, None) => {}
            _ => return Err(InteractiveError::Store),
        }
        Ok(Some(saved))
    }

    /// Clears the selected login even after a partially committed OS write.
    /// Uses only validated non-secret selection, so recovery does not require
    /// reading a damaged or unavailable refresh credential. Deletion failures
    /// are reported after the durable logout tombstone has been published.
    ///
    /// # Errors
    /// Explicit selector conflicts, stale revisions and store failures fail closed.
    pub async fn logout_selected(
        &self,
        options: &super::InteractiveOptions,
    ) -> Result<(), InteractiveError> {
        let saved = self.selection()?.ok_or(InteractiveError::NotConfigured)?;
        let identity = saved
            .identity
            .as_ref()
            .ok_or(InteractiveError::NotConfigured)?;
        if identity.tenant != options.tenant
            || saved.client.as_deref() != Some(options.client.as_str())
            || options
                .account_hint
                .as_ref()
                .is_some_and(|hint| !hint.eq_ignore_ascii_case(&identity.account))
        {
            return Err(InteractiveError::IdentityMismatch);
        }
        self.compare_exchange(saved.revision, None).await?;
        Ok(())
    }

    fn lock(&self) -> Result<File, InteractiveError> {
        reject_links(&self.root)?;
        fs::create_dir_all(&self.root).map_err(|_| InteractiveError::Store)?;
        let path = self.root.join(LOCK);
        reject_links(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| InteractiveError::Store)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => InteractiveError::SessionChanged,
            _ => InteractiveError::Store,
        })?;
        Ok(file)
    }

    fn load_sync(&self) -> Result<SessionSnapshot, InteractiveError> {
        let _lock = self.lock()?;
        let Some(saved) = self.selection()? else {
            return Ok(SessionSnapshot {
                revision: 0,
                session: None,
            });
        };
        if saved.session_id.is_none() {
            return Ok(SessionSnapshot {
                revision: saved.revision,
                session: None,
            });
        }
        let bytes = self.protected_read()?;
        let value = super::json::parse(&bytes).map_err(|_| InteractiveError::Store)?;
        let protected: Protected =
            serde_json::from_value(value).map_err(|_| InteractiveError::Store)?;
        if protected.revision != saved.revision
            || Some(protected.session.session_id) != saved.session_id
            || Some(&protected.session.client) != saved.client.as_ref()
            || Some(&protected.session.identity) != saved.identity.as_ref()
            || protected.session.refresh_token.secret().is_empty()
            || protected.session.refresh_token.secret().len() > 32_768
        {
            return Err(InteractiveError::Store);
        }
        Ok(SessionSnapshot {
            revision: saved.revision,
            session: Some(protected.session),
        })
    }

    fn exchange_sync(
        &self,
        revision: u64,
        session: Option<RefreshSession>,
    ) -> Result<u64, InteractiveError> {
        let _lock = self.lock()?;
        let previous = self.selection()?;
        if previous.as_ref().map_or(0, |saved| saved.revision) != revision {
            return Err(InteractiveError::SessionChanged);
        }
        let next = revision.checked_add(1).ok_or(InteractiveError::Store)?;
        let saved = SavedSelection {
            schema: 1,
            provider: "device-code".into(),
            revision: next,
            locator: self.locator.clone(),
            session_id: session.as_ref().map(|session| session.session_id),
            client: session.as_ref().map(|session| session.client.clone()),
            identity: session.as_ref().map(|session| session.identity.clone()),
        };
        if let Some(session) = session {
            session.identity.validate()?;
            if !super::is_guid(&session.client)
                || session.session_id == 0
                || session.session_id > next
            {
                return Err(InteractiveError::Store);
            }
            let create =
                previous.as_ref().and_then(|saved| saved.session_id) != Some(session.session_id);
            let bytes = serde_json::to_vec(&Protected {
                revision: next,
                session,
            })
            .map_err(|_| InteractiveError::Store)?;
            self.protected_write(&bytes, create)?;
            self.publish(&saved)?;
        } else {
            // Invalidate before deleting the OS secret: a failed delete cannot
            // resurrect a selected session or let an old refresher publish.
            self.publish(&saved)?;
            self.protected_delete()?;
        }
        Ok(next)
    }

    fn protected_read(&self) -> Result<Vec<u8>, InteractiveError> {
        #[cfg(target_os = "macos")]
        return self.mac_exchange(super::macos::ProtectedRequest::read(&self.locator));
        #[cfg(not(target_os = "macos"))]
        native_read(&self.locator)
    }

    fn protected_write(&self, bytes: &[u8], create: bool) -> Result<(), InteractiveError> {
        #[cfg(target_os = "macos")]
        {
            let _ = create;
            self.mac_exchange(super::macos::ProtectedRequest::upsert(&self.locator, bytes))?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        native_write(&self.locator, bytes, create)
    }

    fn protected_delete(&self) -> Result<(), InteractiveError> {
        #[cfg(target_os = "macos")]
        {
            self.mac_exchange(super::macos::ProtectedRequest::delete(&self.locator))?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        native_delete(&self.locator)
    }

    #[cfg(target_os = "macos")]
    fn mac_exchange(
        &self,
        request: super::macos::ProtectedRequest,
    ) -> Result<Vec<u8>, InteractiveError> {
        super::macos::exchange(
            self.helper
                .as_ref()
                .ok_or(InteractiveError::Configuration)?,
            self.operation.as_ref().ok_or(InteractiveError::Store)?,
            request,
        )
    }

    #[cfg(target_os = "macos")]
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(Self) -> Result<T, InteractiveError> + Send + 'static,
    ) -> Result<T, InteractiveError> {
        let mut store = self.clone();
        let deadline = std::time::Instant::now()
            + self
                .helper
                .as_ref()
                .map_or(std::time::Duration::from_secs(30), |h| h.inner.deadline);
        let cancellation = crate::macos_helper::Cancellation::new();
        store.operation = Some(MacOperation {
            deadline,
            cancelled: cancellation.flag(),
        });
        // The blocking owner retains the session lock until child cleanup.
        // Dropping this caller signals cancellation, not detachment of the lock.
        let operation = tokio::task::spawn_blocking(move || work(store));
        tokio::select! {
            result = operation => result.map_err(|_| InteractiveError::Store)?,
            _ = tokio::time::sleep_until(deadline.into()) => Err(InteractiveError::Store),
        }
    }

    fn publish(&self, saved: &SavedSelection) -> Result<(), InteractiveError> {
        let path = self.root.join(".device-code-session.next");
        reject_links(&path)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                fs::remove_file(&path).map_err(|_| InteractiveError::Store)?
            }
            Ok(_) => return Err(InteractiveError::Store),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(InteractiveError::Store),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| InteractiveError::Store)?;
        let result = (|| {
            serde_json::to_writer(&mut file, saved).map_err(|_| InteractiveError::Store)?;
            file.write_all(b"\n").map_err(|_| InteractiveError::Store)?;
            file.sync_all().map_err(|_| InteractiveError::Store)?;
            drop(file);
            fs::rename(&path, self.root.join(SELECTOR)).map_err(|_| InteractiveError::Store)?;
            #[cfg(unix)]
            File::open(&self.root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| InteractiveError::Store)?;
            Ok(())
        })();
        if result.is_err() {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(InteractiveError::Store),
            }
        }
        result
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Protected {
    revision: u64,
    session: RefreshSession,
}

#[async_trait]
impl SessionStore for OsSessionStore {
    async fn load(&self) -> Result<SessionSnapshot, InteractiveError> {
        #[cfg(target_os = "macos")]
        return self.run(|store| store.load_sync()).await;
        #[cfg(not(target_os = "macos"))]
        {
            let store = self.clone();
            tokio::task::spawn_blocking(move || store.load_sync())
                .await
                .map_err(|_| InteractiveError::Store)?
        }
    }
    async fn compare_exchange(
        &self,
        revision: u64,
        session: Option<RefreshSession>,
    ) -> Result<u64, InteractiveError> {
        #[cfg(target_os = "macos")]
        return self
            .run(move |store| store.exchange_sync(revision, session))
            .await;
        #[cfg(not(target_os = "macos"))]
        {
            let store = self.clone();
            tokio::task::spawn_blocking(move || store.exchange_sync(revision, session))
                .await
                .map_err(|_| InteractiveError::Store)?
        }
    }
}

fn reject_links(path: &Path) -> Result<(), InteractiveError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(InteractiveError::Store);
                    }
                }
                if metadata.file_type().is_symlink() {
                    return Err(InteractiveError::Store);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(InteractiveError::Store),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn entry(locator: &str) -> Result<keyring::Entry, InteractiveError> {
    let builder = keyring::windows::default_credential_builder();
    let credential = builder
        .build(None, SERVICE, locator)
        .map_err(|_| InteractiveError::Store)?;
    Ok(keyring::Entry::new_with_credential(credential))
}

#[cfg(windows)]
fn without_ui<T>(
    work: impl FnOnce() -> Result<T, InteractiveError>,
) -> Result<T, InteractiveError> {
    work()
}

#[cfg(windows)]
fn native_read(locator: &str) -> Result<Vec<u8>, InteractiveError> {
    without_ui(|| {
        entry(locator)?
            .get_secret()
            .map_err(|_| InteractiveError::Store)
    })
}
#[cfg(windows)]
fn native_write(locator: &str, bytes: &[u8], _: bool) -> Result<(), InteractiveError> {
    without_ui(|| {
        entry(locator)?
            .set_secret(bytes)
            .map_err(|_| InteractiveError::Store)
    })
}
#[cfg(windows)]
fn native_delete(locator: &str) -> Result<(), InteractiveError> {
    without_ui(|| match entry(locator)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(InteractiveError::Store),
    })
}

#[cfg(target_os = "linux")]
fn linux<T>(
    locator: &str,
    work: impl FnOnce(
        &secret_service::blocking::SecretService<'_>,
        std::collections::HashMap<&str, &str>,
    ) -> Result<T, InteractiveError>,
) -> Result<T, InteractiveError> {
    let service =
        secret_service::blocking::SecretService::connect(secret_service::EncryptionType::Dh)
            .map_err(|_| InteractiveError::Store)?;
    let attributes = std::collections::HashMap::from([
        ("service", SERVICE),
        ("username", locator),
        ("application", SERVICE),
    ]);
    work(&service, attributes)
}

#[cfg(target_os = "linux")]
fn native_read(locator: &str) -> Result<Vec<u8>, InteractiveError> {
    linux(locator, |service, attributes| {
        let found = service
            .search_items(attributes)
            .map_err(|_| InteractiveError::Store)?;
        if !found.locked.is_empty() || found.unlocked.len() != 1 {
            return Err(InteractiveError::Store);
        }
        found.unlocked[0]
            .get_secret()
            .map_err(|_| InteractiveError::Store)
    })
}
#[cfg(target_os = "linux")]
fn native_write(locator: &str, bytes: &[u8], create: bool) -> Result<(), InteractiveError> {
    linux(locator, |service, attributes| {
        let found = service
            .search_items(attributes.clone())
            .map_err(|_| InteractiveError::Store)?;
        if !found.locked.is_empty() || found.unlocked.len() > 1 {
            return Err(InteractiveError::Store);
        }
        if let Some(item) = found.unlocked.first() {
            return item
                .set_secret(bytes, "application/octet-stream")
                .map_err(|_| InteractiveError::Store);
        }
        if !create {
            return Err(InteractiveError::Store);
        }
        let collection = service
            .get_default_collection()
            .map_err(|_| InteractiveError::Store)?;
        if collection.collection_path.as_str().ends_with("/session") {
            return Err(InteractiveError::Store);
        }
        if collection
            .is_locked()
            .map_err(|_| InteractiveError::Store)?
        {
            return Err(InteractiveError::Store);
        }
        let created = no_ui::create_empty(
            &no_ui::Native,
            collection.collection_path.as_str(),
            attributes.clone(),
        )?;
        // The direct boundary creates only an empty marker. Write credentials
        // only after its no-prompt result is checked, through the existing DH
        // session, to the exact newly created item.
        let found = service
            .search_items(attributes)
            .map_err(|_| InteractiveError::Store)?;
        if !found.locked.is_empty()
            || found.unlocked.len() != 1
            || found.unlocked[0].item_path.as_str() != created
        {
            return Err(InteractiveError::Store);
        }
        found.unlocked[0]
            .set_secret(bytes, "application/octet-stream")
            .map_err(|_| InteractiveError::Store)
    })
}
#[cfg(target_os = "linux")]
fn native_delete(locator: &str) -> Result<(), InteractiveError> {
    linux(locator, |service, attributes| {
        let found = service
            .search_items(attributes)
            .map_err(|_| InteractiveError::Store)?;
        if !found.locked.is_empty() || found.unlocked.len() > 1 {
            return Err(InteractiveError::Store);
        }
        if let Some(item) = found.unlocked.first() {
            no_ui::delete(&no_ui::Native, item.item_path.as_str())?;
        }
        Ok(())
    })
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn native_read(_: &str) -> Result<Vec<u8>, InteractiveError> {
    Err(InteractiveError::Unsupported)
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn native_write(_: &str, _: &[u8], _: bool) -> Result<(), InteractiveError> {
    Err(InteractiveError::Unsupported)
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn native_delete(_: &str) -> Result<(), InteractiveError> {
    Err(InteractiveError::Unsupported)
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn store() -> OsSessionStore {
        let root = std::env::current_dir().unwrap().join(format!(
            ".interactive-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!root.exists());
        OsSessionStore::new(root).unwrap()
    }

    fn tombstone(store: &OsSessionStore, revision: u64) -> SavedSelection {
        SavedSelection {
            schema: 1,
            provider: "device-code".into(),
            revision,
            locator: store.locator.clone(),
            session_id: None,
            client: None,
            identity: None,
        }
    }

    #[test]
    fn persisted_logout_revision_and_busy_lock_reject_stale_writers_without_touching_os_store() {
        let store = store();
        let lock = store.lock().unwrap();
        assert!(matches!(
            store.lock(),
            Err(InteractiveError::SessionChanged)
        ));
        store.publish(&tombstone(&store, 7)).unwrap();
        drop(lock);
        assert_eq!(store.selection().unwrap().unwrap().revision, 7);
        let snapshot = store.load_sync().unwrap();
        assert_eq!(snapshot.revision, 7);
        assert!(snapshot.session.is_none());
        assert_eq!(
            store.exchange_sync(0, None).unwrap_err(),
            InteractiveError::SessionChanged
        );
        assert_eq!(
            store.exchange_sync(6, None).unwrap_err(),
            InteractiveError::SessionChanged
        );
        fs::remove_dir_all(store.root).unwrap();
    }

    #[test]
    fn selector_rejects_duplicate_secret_shaped_and_wrong_provider_data_without_native_access() {
        let store = store();
        let lock = store.lock().unwrap();
        store.publish(&tombstone(&store, 1)).unwrap();
        drop(lock);
        let valid = serde_json::to_value(tombstone(&store, 1)).unwrap();
        let mut secret = valid.clone();
        secret["refresh_token"] = "secret-canary".into();
        let mut wrong = valid;
        wrong["provider"] = "broker".into();
        for bytes in [
            serde_json::to_vec(&secret).unwrap(),
            serde_json::to_vec(&wrong).unwrap(),
            br#"{"schema":1,"schema":1}"#.to_vec(),
            vec![b' '; 8193],
        ] {
            fs::write(store.root.join(SELECTOR), bytes).unwrap();
            let error = store.selection().unwrap_err();
            assert!(!format!("{error} {error:?}").contains("canary"));
        }
        fs::remove_dir_all(store.root).unwrap();
    }

    #[test]
    fn stale_nonsecret_staging_file_is_recoverable_under_the_lock() {
        let store = store();
        let lock = store.lock().unwrap();
        fs::write(
            store.root.join(".device-code-session.next"),
            b"interrupted metadata",
        )
        .unwrap();
        store.publish(&tombstone(&store, 3)).unwrap();
        drop(lock);
        assert_eq!(store.selection().unwrap().unwrap().revision, 3);
        assert!(!store.root.join(".device-code-session.next").exists());
        fs::remove_dir_all(store.root).unwrap();
    }
}
