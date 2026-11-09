// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit Linux direct-provider PKCS#11 signing. No discovery, PIN prompting,
//! credential persistence, provider fallback or private-key export.
//!
//! The operator trusts the pinned native module and its loader dependencies as
//! executable code. A hash is an identity check, not a sandbox or proof that a
//! provider will not delegate. Proxy/aggregator modules are outside this contract.
//! Each operation opens the root-custodied module, hashes the retained descriptor,
//! loads through that descriptor, and rechecks its identity and hash after load.
//! Root/administrator modification during loading remains outside the threat model.
//!
//! The CLI's opt-in `certificate-store-pkcs11` mode captures the following inputs
//! once; this SDK module never reads environment variables:
//!
//! | Variable | Required value |
//! | --- | --- |
//! | `FAB_RUST_PKCS11_PROVIDER_KIND` | `direct`, explicit operator trust |
//! | `FAB_RUST_PKCS11_MODULE` | Canonical absolute Linux module path |
//! | `FAB_RUST_PKCS11_MODULE_SHA256` | 64 hexadecimal SHA-256 digits |
//! | `FAB_RUST_PKCS11_SLOT` | Exact unsigned decimal slot ID |
//! | `FAB_RUST_PKCS11_TOKEN_MANUFACTURER` | Exact manufacturer, at most 32 bytes |
//! | `FAB_RUST_PKCS11_TOKEN_MODEL` | Exact model, at most 16 bytes |
//! | `FAB_RUST_PKCS11_TOKEN_SERIAL` | Exact serial, at most 16 bytes |
//! | `FAB_RUST_CERTIFICATE_SHA256` | 64 hexadecimal digits over the complete DER |
//! | `FAB_RUST_PKCS11_CKA_ID` | 1–128 bytes, encoded as even-length hexadecimal |
//! | `FAB_RUST_PKCS11_AUTHENTICATION` | `pin` or `protected-path` |
//! | `FAB_RUST_PKCS11_PIN` | Explicit snapshot for `pin`; absent for `protected-path` |
//!
//! Tenant/application inputs and Azure's assertion exchange use the existing
//! headless credential contract. No module/PIN is persisted or placed in batch
//! diagnostics. Public binding metadata changes with every selection component.
//!
//! Module and ancestors must be root-owned and not group/other writable.
//! `/proc/self/fd` must be available. The module must support OS locking without
//! starting its own threads; PIN mode rejects tokens advertising a protected path,
//! and keys requiring per-use context authentication are explicitly unsupported.
//! RSA-2048 through RSA-4096 matches the existing Windows assertion policy.
//! cryptoki's safe facade controls allocations inside provider calls; this is not
//! a hostile-provider memory sandbox. Application searches/attributes/input and
//! waiting are bounded, but a hung native call can also delay caller-runtime
//! shutdown. Only fake-provider and public-key tests are supplied: no live token,
//! provider loading, PIN discovery, certificate enrollment or OAuth certification
//! is established by this implementation.
//!
//! Integration uses the default-off `certificate-store-pkcs11` feature, selecting
//! `headless`, the existing `ring`, and Linux-only optional `cryptoki = "=0.12.1"`
//! and `x509-parser = "=0.18.1"` dependencies, both with default features disabled.
//! The CLI feature forwards to `fabric-identity/certificate-store-pkcs11` and
//! `headless`. Dependency/features/lockfile changes belong to the integration owner.
//! API evidence: [cryptoki 0.12.1 source](https://github.com/parallaxsecond/rust-cryptoki/tree/cryptoki-0.12.1/cryptoki/src)
//! and [x509-parser 0.18.1 documentation](https://docs.rs/x509-parser/0.18.1/x509_parser/).

use super::HeadlessError;
use super::certificate::{CertificateError, CertificateSigner};
use async_trait::async_trait;
use azure_core::credentials::Secret;
use cryptoki::{
    context::{CInitializeArgs, CInitializeFlags, Function, Pkcs11},
    error::{Error as Pkcs11Error, RvError},
    mechanism::{
        Mechanism, MechanismType,
        rsa::{PkcsMgfType, PkcsPssParams},
    },
    object::{
        Attribute, AttributeInfo, AttributeType, CertificateType, KeyType, ObjectClass,
        ObjectHandle,
    },
    session::{Session, UserType},
    slot::Slot,
    types::AuthPin,
};
use ring::{
    digest::{Context, SHA256, digest},
    rand::{SecureRandom, SystemRandom},
    signature::{RSA_PSS_2048_8192_SHA256, RsaPublicKeyComponents},
};
use std::{
    fmt,
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom},
    num::NonZeroUsize,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use x509_parser::{
    certificate::Validity,
    prelude::*,
    public_key::{PublicKey, RSAPublicKey},
};

const INPUT_LIMIT: usize = 8192;
const MODULE_LIMIT: u64 = 64 * 1024 * 1024;
const CERTIFICATE_LIMIT: usize = 64 * 1024;
const KEY_ID_LIMIT: usize = 128;
const PIN_LIMIT: usize = 1024;
const OBJECT_LIMIT: usize = 64;
const MAX_WAIT: Duration = Duration::from_secs(120);

/// A configuration field name, never the supplied value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pkcs11Field {
    /// Canonical absolute direct-provider module path.
    Module,
    /// Explicit slot identifier, always revalidated against the token identity.
    Slot,
    /// Exact token manufacturer.
    Manufacturer,
    /// Exact token model.
    Model,
    /// Exact token serial number.
    Serial,
    /// Nonempty hexadecimal CKA_ID.
    KeyId,
    /// Explicit PIN source or protected-path policy.
    Authentication,
    /// Nonzero bounded local waiting deadline.
    Deadline,
}

/// Invalid explicit selection, retaining only a field identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid explicit PKCS#11 configuration field: {field:?}")]
pub struct Pkcs11ConfigError {
    /// The invalid field, without input values or source errors.
    pub field: Pkcs11Field,
}

fn invalid(field: Pkcs11Field) -> Pkcs11ConfigError {
    Pkcs11ConfigError { field }
}

fn failure(error: CertificateError) -> HeadlessError {
    HeadlessError::Certificate(error)
}

/// Exact space-padding-normalized PKCS#11 token identity.
#[derive(Clone, Eq, PartialEq)]
pub struct Pkcs11Token {
    manufacturer: String,
    model: String,
    serial: String,
}

impl Pkcs11Token {
    /// Captures an exact token identity, without enumerating slots or devices.
    ///
    /// # Errors
    /// Rejects empty, overlong or control-containing token fields.
    pub fn new(manufacturer: &str, model: &str, serial: &str) -> Result<Self, Pkcs11ConfigError> {
        Ok(Self {
            manufacturer: token_field(manufacturer, 32, Pkcs11Field::Manufacturer)?,
            model: token_field(model, 16, Pkcs11Field::Model)?,
            serial: token_field(serial, 16, Pkcs11Field::Serial)?,
        })
    }

    fn matches(&self, manufacturer: &str, model: &str, serial: &str) -> bool {
        self.manufacturer == manufacturer.trim_end_matches(' ')
            && self.model == model.trim_end_matches(' ')
            && self.serial == serial.trim_end_matches(' ')
    }
}

impl fmt::Debug for Pkcs11Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkcs11Token").finish_non_exhaustive()
    }
}

fn token_field(value: &str, limit: usize, field: Pkcs11Field) -> Result<String, Pkcs11ConfigError> {
    let normalized = value.trim_end_matches(' ');
    if value.len() > limit
        || normalized.is_empty()
        || value
            .chars()
            .any(|character| character.is_control() || character == '\u{fffd}')
    {
        return Err(invalid(field));
    }
    Ok(normalized.to_owned())
}

/// Inert direct-provider, slot, exact token and certificate/key association.
///
/// The caller attests that the module is a direct trusted provider, not p11-kit,
/// an aggregator, or a provider whose user configuration loads additional modules.
/// A slot is mandatory: cryptoki's slot-list API allocates and retries internally
/// without a caller bound. This adapter instead makes one exact slot query, never
/// enumerates registered providers, and never chooses the first available token.
#[derive(Clone)]
pub struct Pkcs11Selection {
    module: PathBuf,
    module_sha256: [u8; 32],
    slot: u64,
    token: Pkcs11Token,
    certificate_sha256: [u8; 32],
    key_id: Vec<u8>,
}

impl Pkcs11Selection {
    /// Captures public selection metadata; does not touch the filesystem or module.
    ///
    /// # Errors
    /// Rejects nonabsolute/noncanonical lexical paths, known proxy module names,
    /// invalid slot widths and empty or overlong CKA_ID values.
    pub fn new(
        module: PathBuf,
        module_sha256: [u8; 32],
        slot: u64,
        token: Pkcs11Token,
        certificate_sha256: [u8; 32],
        key_id: Vec<u8>,
    ) -> Result<Self, Pkcs11ConfigError> {
        validate_module_path(&module)?;
        Slot::try_from(slot).map_err(|_| invalid(Pkcs11Field::Slot))?;
        if key_id.is_empty() || key_id.len() > KEY_ID_LIMIT {
            return Err(invalid(Pkcs11Field::KeyId));
        }
        Ok(Self {
            module,
            module_sha256,
            slot,
            token,
            certificate_sha256,
            key_id,
        })
    }

    /// Returns public selection binding metadata, excluding PINs and callbacks.
    ///
    /// Hashing the length-delimited complete selection prevents batch reuse across
    /// module, slot, token, fingerprint or key-ID changes without publishing them.
    pub fn binding(&self) -> String {
        use std::fmt::Write as _;
        let mut hash = Context::new(&SHA256);
        for value in [
            self.module.as_os_str().as_encoded_bytes(),
            self.module_sha256.as_slice(),
            self.slot.to_be_bytes().as_slice(),
            self.token.manufacturer.as_bytes(),
            self.token.model.as_bytes(),
            self.token.serial.as_bytes(),
            self.certificate_sha256.as_slice(),
            self.key_id.as_slice(),
        ] {
            hash.update(&(value.len() as u64).to_be_bytes());
            hash.update(value);
        }
        let mut binding = String::from("linux-pkcs11-direct:");
        for byte in hash.finish().as_ref() {
            let _ = write!(binding, "{byte:02x}");
        }
        binding
    }
}

impl fmt::Debug for Pkcs11Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkcs11Selection").finish_non_exhaustive()
    }
}

fn validate_module_path(path: &Path) -> Result<(), Pkcs11ConfigError> {
    let text = path.to_str().ok_or_else(|| invalid(Pkcs11Field::Module))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !path.is_absolute()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || text.contains("//")
        || text.contains("/./")
        || !path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        || name.is_empty()
        || name.to_ascii_lowercase().contains("p11-kit")
        || name.to_ascii_lowercase().contains("proxy")
    {
        return Err(invalid(Pkcs11Field::Module));
    }
    Ok(())
}

/// Explicit caller-owned PIN callback. Called only on the owned blocking worker.
///
/// Implementations must not prompt, discover a PIN, read ambient configuration or
/// retain diagnostic copies. A caller may return a previously captured secret.
pub trait Pkcs11PinSource: Send + Sync {
    /// Supplies a PIN for this operation, never a fallback for protected-path mode.
    ///
    /// # Errors
    /// Returns a sanitized failure when the caller's explicit PIN is unavailable.
    fn pin(&self) -> Result<Secret, HeadlessError>;
}

impl<F> Pkcs11PinSource for F
where
    F: Fn() -> Result<Secret, HeadlessError> + Send + Sync,
{
    fn pin(&self) -> Result<Secret, HeadlessError> {
        self()
    }
}

/// Exactly one operator-selected authentication policy.
#[derive(Clone)]
pub enum Pkcs11Authentication {
    /// Use only this caller-injected PIN callback.
    Pin(Arc<dyn Pkcs11PinSource>),
    /// Permit the selected token's advertised protected authentication path.
    ///
    /// This explicitly permits provider-owned physical interaction. It never
    /// means "try a default PIN" or "discover an operating-system credential".
    ProtectedPath,
}

impl fmt::Debug for Pkcs11Authentication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pin(_) => "Pkcs11Authentication::Pin(<redacted>)",
            Self::ProtectedPath => "Pkcs11Authentication::ProtectedPath",
        })
    }
}

/// Explicit Linux PKCS#11 signer, with independent globally serialized capacity.
///
/// All instances share one worker permit, including cleanup. A timed-out foreign
/// operation cannot be killed safely; its permit remains occupied until foreign
/// work and cleanup actually finish. This also prevents this backend from calling
/// global C_Finalize while another instance has a live context. Other independent
/// PKCS#11 consumers must not share this module in the process.
pub struct Pkcs11Signer {
    selection: Pkcs11Selection,
    authentication: Pkcs11Authentication,
    wait: Duration,
    parent_deadline: Option<Instant>,
    capacity: Arc<Semaphore>,
}

impl Pkcs11Signer {
    /// Captures configuration without loading a provider, reading a PIN or signing.
    ///
    /// # Errors
    /// Rejects a zero deadline or one exceeding the two-minute waiting bound.
    pub fn new(
        selection: Pkcs11Selection,
        authentication: Pkcs11Authentication,
        wait: Duration,
    ) -> Result<Self, Pkcs11ConfigError> {
        if wait.is_zero() || wait > MAX_WAIT {
            return Err(invalid(Pkcs11Field::Deadline));
        }
        static CAPACITY: OnceLock<Arc<Semaphore>> = OnceLock::new();
        Ok(Self {
            selection,
            authentication,
            wait,
            parent_deadline: None,
            capacity: CAPACITY.get_or_init(|| Arc::new(Semaphore::new(1))).clone(),
        })
    }

    /// Applies an earlier absolute caller-owned deadline without native work.
    ///
    /// An expired deadline fails before reserving native capacity. Dropping the
    /// waiting future also invalidates publication cooperatively; neither action
    /// claims to terminate a synchronous provider call.
    pub fn with_parent_deadline(mut self, deadline: Instant) -> Self {
        self.parent_deadline = Some(deadline);
        self
    }

    /// Returns batch identity binding, including the selected authentication policy.
    pub fn binding(&self) -> String {
        format!(
            "{}:{}",
            self.selection.binding(),
            match self.authentication {
                Pkcs11Authentication::Pin(_) => "pin",
                Pkcs11Authentication::ProtectedPath => "protected-path",
            }
        )
    }
}

impl fmt::Debug for Pkcs11Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkcs11Signer").finish_non_exhaustive()
    }
}

#[async_trait]
impl CertificateSigner for Pkcs11Signer {
    fn certificate_sha256(&self) -> [u8; 32] {
        self.selection.certificate_sha256
    }

    fn assertion_nonce(&self) -> Result<[u8; 16], HeadlessError> {
        let mut nonce = [0; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| HeadlessError::Unavailable)?;
        Ok(nonce)
    }

    async fn sign_ps256(&self, signing_input: &[u8]) -> Result<Vec<u8>, HeadlessError> {
        if signing_input.is_empty() || signing_input.len() > INPUT_LIMIT {
            return Err(HeadlessError::Configuration);
        }
        let selection = self.selection.clone();
        let authentication = self.authentication.clone();
        let input = signing_input.to_vec();
        let deadline = Instant::now()
            .checked_add(self.wait)
            .ok_or(HeadlessError::Configuration)?;
        let deadline = self
            .parent_deadline
            .map_or(deadline, |parent| parent.min(deadline));
        run_worker(self.capacity.clone(), deadline, move |control| {
            sign_operation(&selection, &authentication, &input, control)
        })
        .await
    }
}

struct Control {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl Control {
    fn checkpoint(&self) -> Result<(), HeadlessError> {
        if self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(failure(CertificateError::Cancelled));
        }
        Ok(())
    }
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

async fn run_worker(
    capacity: Arc<Semaphore>,
    deadline: Instant,
    operation: impl FnOnce(&Control) -> Result<Vec<u8>, HeadlessError> + Send + 'static,
) -> Result<Vec<u8>, HeadlessError> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| HeadlessError::Unavailable)?;
    if Instant::now() >= deadline {
        return Err(failure(CertificateError::Cancelled));
    }
    let permit = capacity
        .try_acquire_owned()
        .map_err(|_| failure(CertificateError::Capacity))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(cancelled.clone());
    let control = Control {
        cancelled,
        deadline,
    };
    let worker = runtime.spawn_blocking(move || {
        let mut permit = WorkerPermit {
            permit: Some(permit),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            control.checkpoint()?;
            let result = operation(&control)?;
            control.checkpoint()?;
            Ok(result)
        }));
        match result {
            Ok(result) => {
                if matches!(
                    result,
                    Err(HeadlessError::Certificate(CertificateError::CleanupPending))
                ) {
                    permit.quarantine();
                }
                result
            }
            Err(_) => {
                permit.quarantine();
                Err(failure(CertificateError::CleanupPending))
            }
        }
    });
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), worker).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => Err(failure(CertificateError::CleanupPending)),
    }
}

struct WorkerPermit {
    permit: Option<OwnedSemaphorePermit>,
}

impl WorkerPermit {
    fn quarantine(&mut self) {
        if let Some(permit) = self.permit.take() {
            permit.forget();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModuleStamp {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    uid: u32,
    mode: u32,
    links: u64,
}

impl ModuleStamp {
    fn read(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
            uid: metadata.uid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
        }
    }
}

fn root_custody(uid: u32, mode: u32) -> bool {
    uid == 0 && mode & 0o022 == 0
}

fn module_hash(file: &mut File, control: &Control) -> Result<[u8; 32], HeadlessError> {
    hash_module_reader(file, control, MODULE_LIMIT)
}

fn hash_module_reader(
    file: &mut (impl Read + Seek),
    control: &Control,
    limit: u64,
) -> Result<[u8; 32], HeadlessError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| failure(CertificateError::Module))?;
    let mut hash = Context::new(&SHA256);
    let mut total = 0u64;
    let mut buffer = [0; 16 * 1024];
    loop {
        control.checkpoint()?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| failure(CertificateError::Module))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > limit {
            return Err(failure(CertificateError::Module));
        }
        hash.update(&buffer[..count]);
    }
    hash.finish()
        .as_ref()
        .try_into()
        .map_err(|_| failure(CertificateError::Module))
}

struct LoadedModule {
    context: Option<Pkcs11>,
    initialized: bool,
    file: Option<File>,
    stamp: ModuleStamp,
}

impl LoadedModule {
    fn open(selection: &Pkcs11Selection, control: &Control) -> Result<Self, HeadlessError> {
        control.checkpoint()?;
        let rejected = || failure(CertificateError::Module);
        let canonical = fs::canonicalize(&selection.module).map_err(|_| rejected())?;
        if canonical != selection.module {
            return Err(rejected());
        }
        for ancestor in selection.module.ancestors().skip(1) {
            let metadata = fs::symlink_metadata(ancestor).map_err(|_| rejected())?;
            if !metadata.is_dir() || !root_custody(metadata.uid(), metadata.mode()) {
                return Err(rejected());
            }
        }
        let metadata = fs::symlink_metadata(&selection.module).map_err(|_| rejected())?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MODULE_LIMIT
            || !root_custody(metadata.uid(), metadata.mode())
        {
            return Err(rejected());
        }
        let stamp = ModuleStamp::read(&metadata);
        let mut file = File::open(&selection.module).map_err(|_| rejected())?;
        if ModuleStamp::read(&file.metadata().map_err(|_| rejected())?) != stamp
            || module_hash(&mut file, control)? != selection.module_sha256
        {
            return Err(rejected());
        }
        control.checkpoint()?;
        // The retained descriptor, not a second resolution of the configured path,
        // identifies the inode loaded even if the original name is replaced.
        let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let context = Pkcs11::new(descriptor_path).map_err(|_| rejected())?;
        let mut loaded = Self {
            context: Some(context),
            initialized: false,
            file: Some(file),
            stamp,
        };
        loaded.revalidate(selection, control)?;
        loaded
            .context()?
            .initialize(CInitializeArgs::new(
                CInitializeFlags::OS_LOCKING_OK | CInitializeFlags::LIBRARY_CANT_CREATE_OS_THREADS,
            ))
            .map_err(|_| failure(CertificateError::Policy))?;
        loaded.initialized = true;
        Ok(loaded)
    }

    fn context(&self) -> Result<&Pkcs11, HeadlessError> {
        self.context
            .as_ref()
            .ok_or_else(|| failure(CertificateError::CleanupPending))
    }

    fn revalidate(
        &mut self,
        selection: &Pkcs11Selection,
        control: &Control,
    ) -> Result<(), HeadlessError> {
        let rejected = || failure(CertificateError::Module);
        let file = self.file.as_mut().ok_or_else(rejected)?;
        if ModuleStamp::read(&file.metadata().map_err(|_| rejected())?) != self.stamp
            || module_hash(file, control)? != selection.module_sha256
            || ModuleStamp::read(&file.metadata().map_err(|_| rejected())?) != self.stamp
            || ModuleStamp::read(&fs::symlink_metadata(&selection.module).map_err(|_| rejected())?)
                != self.stamp
        {
            return Err(rejected());
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(), HeadlessError> {
        if self.initialized {
            if self.context()?.clone().finalize().is_err() {
                // Retain the loaded library and descriptor rather than unloading
                // code that may still own native state after failed C_Finalize.
                self.initialized = false;
                std::mem::forget(self);
                return Err(failure(CertificateError::CleanupPending));
            }
            self.initialized = false;
        }
        Ok(())
    }
}

impl Drop for LoadedModule {
    fn drop(&mut self) {
        if self.initialized
            && let Some(context) = self.context.take()
            && context.clone().finalize().is_err()
        {
            std::mem::forget(context);
            if let Some(file) = self.file.take() {
                std::mem::forget(file);
            }
        }
    }
}

fn sign_operation(
    selection: &Pkcs11Selection,
    authentication: &Pkcs11Authentication,
    input: &[u8],
    control: &Control,
) -> Result<Vec<u8>, HeadlessError> {
    let mut loaded = LoadedModule::open(selection, control)?;
    let result = (|| {
        let context = loaded.context()?;
        control.checkpoint()?;
        let slot =
            Slot::try_from(selection.slot).map_err(|_| failure(CertificateError::Selection))?;
        let token = context
            .get_token_info(slot)
            .map_err(|_| failure(CertificateError::Selection))?;
        if !selection.token.matches(
            token.manufacturer_id(),
            token.model(),
            token.serial_number(),
        ) || !token.token_initialized()
        {
            return Err(failure(CertificateError::Selection));
        }
        validate_authentication(authentication, token.protected_authentication_path())?;
        control.checkpoint()?;
        let session = context
            .open_ro_session(slot)
            .map_err(|_| failure(CertificateError::Provider))?;
        let mut logged_in = false;
        let result = (|| {
            let pin = match authentication {
                Pkcs11Authentication::Pin(source) => {
                    let secret = source
                        .pin()
                        .map_err(|_| failure(CertificateError::PinUnavailable))?;
                    if secret.secret().is_empty() || secret.secret().len() > PIN_LIMIT {
                        return Err(failure(CertificateError::PinUnavailable));
                    }
                    Some(AuthPin::new(secret.secret().to_owned().into()))
                }
                Pkcs11Authentication::ProtectedPath => None,
            };
            control.checkpoint()?;
            session
                .login(UserType::User, pin.as_ref())
                .map_err(|_| failure(CertificateError::Authentication))?;
            logged_in = true;
            drop(pin);
            control.checkpoint()?;
            let certificate = select_certificate(&session, selection, control)?;
            let key = select_key(&session, selection, &certificate, control)?;
            let plan = signing_plan(context, slot, certificate.bits)?;
            control.checkpoint()?;
            let bytes = plan.input(input);
            let signature = session
                .sign(&plan.mechanism(), key, &bytes)
                .map_err(|_| failure(CertificateError::Provider))?;
            control.checkpoint()?;
            certificate.verify(input, &signature)?;
            Ok(signature)
        })();
        // Cleanup is intentionally not cancelled and runs on this same worker.
        let logout = if logged_in { session.logout() } else { Ok(()) };
        let close = session.close();
        if logout.is_err() || close.is_err() {
            return Err(failure(CertificateError::CleanupPending));
        }
        result
    })();
    let integrity = loaded.revalidate(selection, control);
    let cleanup = loaded.finish();
    cleanup?;
    if matches!(
        result,
        Err(HeadlessError::Certificate(CertificateError::CleanupPending))
    ) {
        return result;
    }
    integrity?;
    result
}

fn validate_authentication(
    policy: &Pkcs11Authentication,
    protected_path: bool,
) -> Result<(), HeadlessError> {
    match policy {
        Pkcs11Authentication::Pin(_) if protected_path => Err(failure(CertificateError::Policy)),
        Pkcs11Authentication::ProtectedPath if !protected_path => {
            Err(failure(CertificateError::Policy))
        }
        _ => Ok(()),
    }
}

fn bounded_objects(
    session: &Session,
    template: &[Attribute],
    control: &Control,
) -> Result<Vec<ObjectHandle>, HeadlessError> {
    let cache = NonZeroUsize::new(1).ok_or(HeadlessError::Configuration)?;
    let objects = session
        .iter_objects_with_cache_size(template, cache)
        .map_err(|_| failure(CertificateError::Provider))?;
    bounded_collect(
        objects.map(|object| object.map_err(|_| failure(CertificateError::Provider))),
        control,
    )
}

fn bounded_collect<T>(
    mut objects: impl Iterator<Item = Result<T, HeadlessError>>,
    control: &Control,
) -> Result<Vec<T>, HeadlessError> {
    let mut result = Vec::new();
    for _ in 0..=OBJECT_LIMIT {
        control.checkpoint()?;
        match objects.next() {
            Some(Ok(object)) if result.len() < OBJECT_LIMIT => result.push(object),
            Some(Ok(_)) => return Err(failure(CertificateError::SearchLimit)),
            Some(Err(error)) => return Err(error),
            None => return Ok(result),
        }
    }
    Err(failure(CertificateError::SearchLimit))
}

fn attributes(
    session: &Session,
    object: ObjectHandle,
    requested: &[AttributeType],
    control: &Control,
) -> Result<Vec<Attribute>, HeadlessError> {
    control.checkpoint()?;
    let info = session
        .get_attribute_info(object, requested)
        .map_err(|_| failure(CertificateError::Policy))?;
    if info.len() != requested.len()
        || info.iter().zip(requested).any(|(info, attribute)| {
            !matches!(info, AttributeInfo::Available(size) if *size <= attribute_limit(*attribute))
        })
    {
        return Err(failure(CertificateError::Policy));
    }
    control.checkpoint()?;
    let values = session
        .get_attributes(object, requested)
        .map_err(|_| failure(CertificateError::Policy))?;
    validate_attribute_completeness(&values, requested)?;
    Ok(values)
}

fn attribute_limit(attribute: AttributeType) -> usize {
    match attribute {
        AttributeType::Value => CERTIFICATE_LIMIT,
        AttributeType::Modulus => 512,
        AttributeType::PublicExponent => 8,
        AttributeType::Id => KEY_ID_LIMIT,
        _ => 8,
    }
}

fn validate_attribute_completeness(
    values: &[Attribute],
    requested: &[AttributeType],
) -> Result<(), HeadlessError> {
    if values.len() != requested.len()
        || requested.iter().any(|kind| {
            values
                .iter()
                .filter(|value| value.attribute_type() == *kind)
                .count()
                != 1
        })
    {
        return Err(failure(CertificateError::Policy));
    }
    Ok(())
}

struct CertificatePublicKey {
    modulus: Vec<u8>,
    exponent: Vec<u8>,
    bits: usize,
    validity: Validity,
}

impl CertificatePublicKey {
    fn parse(der: &[u8], expected: &[u8; 32]) -> Result<Self, HeadlessError> {
        if der.is_empty()
            || der.len() > CERTIFICATE_LIMIT
            || digest(&SHA256, der).as_ref() != expected
        {
            return Err(failure(CertificateError::Selection));
        }
        let (remaining, certificate) =
            X509Certificate::from_der(der).map_err(|_| failure(CertificateError::Selection))?;
        if !remaining.is_empty() || !certificate.validity().is_valid() {
            return Err(failure(CertificateError::Selection));
        }
        let usage = certificate
            .key_usage()
            .map_err(|_| failure(CertificateError::Policy))?;
        if usage.is_some_and(|usage| !usage.value.digital_signature()) {
            return Err(failure(CertificateError::Policy));
        }
        let PublicKey::RSA(key) = certificate
            .public_key()
            .parsed()
            .map_err(|_| failure(CertificateError::UnsupportedKey))?
        else {
            return Err(failure(CertificateError::UnsupportedKey));
        };
        let public_bits = &certificate.public_key().subject_public_key;
        let (remaining, _) = RSAPublicKey::from_der(public_bits.data.as_ref())
            .map_err(|_| failure(CertificateError::UnsupportedKey))?;
        if !remaining.is_empty() || public_bits.unused_bits != 0 {
            return Err(failure(CertificateError::UnsupportedKey));
        }
        let modulus = unsigned_der_integer(key.modulus)?;
        let exponent = unsigned_der_integer(key.exponent)?;
        let bits = modulus.len() * 8
            - modulus
                .first()
                .map_or(8, |byte| byte.leading_zeros() as usize);
        if !(2048..=4096).contains(&bits) || exponent.is_empty() || exponent.len() > 8 {
            return Err(failure(CertificateError::UnsupportedKey));
        }
        Ok(Self {
            modulus: modulus.to_vec(),
            exponent: exponent.to_vec(),
            bits,
            validity: certificate.validity().clone(),
        })
    }

    fn verify(&self, input: &[u8], signature: &[u8]) -> Result<(), HeadlessError> {
        if !self.validity.is_valid() || signature.len() != self.modulus.len() {
            return Err(failure(CertificateError::Signature));
        }
        RsaPublicKeyComponents {
            n: &self.modulus,
            e: &self.exponent,
        }
        .verify(&RSA_PSS_2048_8192_SHA256, input, signature)
        .map_err(|_| failure(CertificateError::Signature))
    }
}

fn unsigned_der_integer(bytes: &[u8]) -> Result<&[u8], HeadlessError> {
    match bytes {
        [0, rest @ ..] if !rest.is_empty() && rest[0] & 0x80 != 0 => Ok(rest),
        [first, ..] if *first != 0 && first & 0x80 == 0 => Ok(bytes),
        _ => Err(failure(CertificateError::UnsupportedKey)),
    }
}

fn select_certificate(
    session: &Session,
    selection: &Pkcs11Selection,
    control: &Control,
) -> Result<CertificatePublicKey, HeadlessError> {
    let handles = bounded_objects(
        session,
        &[
            Attribute::Class(ObjectClass::CERTIFICATE),
            Attribute::CertificateType(CertificateType::X_509),
            Attribute::Id(selection.key_id.clone()),
            Attribute::Token(true),
        ],
        control,
    )?;
    let mut matched = None;
    for handle in handles {
        let values = attributes(
            session,
            handle,
            &[
                AttributeType::Class,
                AttributeType::CertificateType,
                AttributeType::Id,
                AttributeType::Token,
                AttributeType::Value,
            ],
            control,
        )?;
        let der = certificate_der(&values, &selection.key_id)?;
        record_certificate_match(&mut matched, der, &selection.certificate_sha256)?;
    }
    matched.ok_or_else(|| failure(CertificateError::Selection))
}

fn record_certificate_match(
    matched: &mut Option<CertificatePublicKey>,
    der: &[u8],
    expected: &[u8; 32],
) -> Result<(), HeadlessError> {
    if digest(&SHA256, der).as_ref() == expected {
        if matched.is_some() {
            return Err(failure(CertificateError::Selection));
        }
        *matched = Some(CertificatePublicKey::parse(der, expected)?);
    }
    Ok(())
}

fn certificate_der<'a>(values: &'a [Attribute], key_id: &[u8]) -> Result<&'a [u8], HeadlessError> {
    if !values.contains(&Attribute::Class(ObjectClass::CERTIFICATE))
        || !values.contains(&Attribute::CertificateType(CertificateType::X_509))
        || !values.contains(&Attribute::Token(true))
        || !values.contains(&Attribute::Id(key_id.to_vec()))
    {
        return Err(failure(CertificateError::Selection));
    }
    values
        .iter()
        .find_map(|value| match value {
            Attribute::Value(der) if !der.is_empty() && der.len() <= CERTIFICATE_LIMIT => {
                Some(der.as_slice())
            }
            _ => None,
        })
        .ok_or_else(|| failure(CertificateError::Selection))
}

const KEY_ATTRIBUTES: &[AttributeType] = &[
    AttributeType::Class,
    AttributeType::KeyType,
    AttributeType::Id,
    AttributeType::Token,
    AttributeType::Private,
    AttributeType::Sign,
    AttributeType::Extractable,
    AttributeType::NeverExtractable,
    AttributeType::Sensitive,
    AttributeType::AlwaysSensitive,
    AttributeType::AlwaysAuthenticate,
    AttributeType::Modulus,
    AttributeType::PublicExponent,
];

fn select_key(
    session: &Session,
    selection: &Pkcs11Selection,
    certificate: &CertificatePublicKey,
    control: &Control,
) -> Result<ObjectHandle, HeadlessError> {
    let handles = bounded_objects(
        session,
        &[
            Attribute::Class(ObjectClass::PRIVATE_KEY),
            Attribute::Id(selection.key_id.clone()),
            Attribute::Token(true),
        ],
        control,
    )?;
    let handle = unique_object(&handles)?;
    let values = attributes(session, *handle, KEY_ATTRIBUTES, control)?;
    validate_key_attributes(&values, &selection.key_id, certificate)?;
    Ok(*handle)
}

fn unique_object<T>(objects: &[T]) -> Result<&T, HeadlessError> {
    match objects {
        [object] => Ok(object),
        _ => Err(failure(CertificateError::Selection)),
    }
}

fn validate_key_attributes(
    values: &[Attribute],
    key_id: &[u8],
    certificate: &CertificatePublicKey,
) -> Result<(), HeadlessError> {
    validate_attribute_completeness(values, KEY_ATTRIBUTES)?;
    for required in [
        Attribute::Class(ObjectClass::PRIVATE_KEY),
        Attribute::KeyType(KeyType::RSA),
        Attribute::Id(key_id.to_vec()),
        Attribute::Token(true),
        Attribute::Private(true),
        Attribute::Sign(true),
        Attribute::Extractable(false),
        Attribute::NeverExtractable(true),
        Attribute::Sensitive(true),
        Attribute::AlwaysSensitive(true),
        Attribute::AlwaysAuthenticate(false),
        Attribute::Modulus(certificate.modulus.clone()),
        Attribute::PublicExponent(certificate.exponent.clone()),
    ] {
        if !values.contains(&required) {
            return Err(failure(CertificateError::Policy));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SigningPlan {
    Message,
    Digest,
}

impl SigningPlan {
    fn mechanism(self) -> Mechanism<'static> {
        let parameters = PkcsPssParams {
            hash_alg: MechanismType::SHA256,
            mgf: PkcsMgfType::MGF1_SHA256,
            s_len: cryptoki::types::Ulong::new(32),
        };
        match self {
            Self::Message => Mechanism::Sha256RsaPkcsPss(parameters),
            Self::Digest => Mechanism::RsaPkcsPss(parameters),
        }
    }

    fn input(self, message: &[u8]) -> Vec<u8> {
        match self {
            Self::Message => message.to_vec(),
            Self::Digest => digest(&SHA256, message).as_ref().to_vec(),
        }
    }
}

fn signing_plan(context: &Pkcs11, slot: Slot, bits: usize) -> Result<SigningPlan, HeadlessError> {
    // Two exact mechanism queries avoid the facade's unbounded mechanism-list allocation.
    for (kind, plan) in [
        (MechanismType::SHA256_RSA_PKCS_PSS, SigningPlan::Message),
        (MechanismType::RSA_PKCS_PSS, SigningPlan::Digest),
    ] {
        match context.get_mechanism_info(slot, kind) {
            Ok(info)
                if info.sign() && bits >= info.min_key_size() && bits <= info.max_key_size() =>
            {
                return Ok(plan);
            }
            Ok(_)
            | Err(Pkcs11Error::Pkcs11(RvError::MechanismInvalid, Function::GetMechanismInfo)) => {}
            Err(_) => return Err(failure(CertificateError::Provider)),
        }
    }
    Err(failure(CertificateError::UnsupportedKey))
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::super::public_certificate_vectors as public_vectors;
    use super::*;
    use std::{
        io::Cursor,
        sync::{Condvar, Mutex},
    };

    fn token() -> Pkcs11Token {
        Pkcs11Token::new("Synthetic vendor", "Test model", "Serial 1").unwrap()
    }

    fn selection() -> Pkcs11Selection {
        Pkcs11Selection::new(
            PathBuf::from("/opt/synthetic/libpkcs11.so"),
            [0xab; 32],
            17,
            token(),
            [0xcd; 32],
            vec![1, 2],
        )
        .unwrap()
    }

    fn control() -> Control {
        Control {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    fn pin_policy() -> Pkcs11Authentication {
        Pkcs11Authentication::Pin(Arc::new(|| Ok(Secret::new("synthetic-secret-pin"))))
    }

    #[test]
    fn selector_is_inert_exact_and_fully_bound() {
        let selected = selection();
        let original = selected.binding();
        assert_eq!(original, selected.clone().binding());
        for changed in [
            Pkcs11Selection {
                module: PathBuf::from("/opt/other/libpkcs11.so"),
                ..selected.clone()
            },
            Pkcs11Selection {
                module_sha256: [0; 32],
                ..selected.clone()
            },
            Pkcs11Selection {
                slot: 18,
                ..selected.clone()
            },
            Pkcs11Selection {
                token: Pkcs11Token::new("Other", "Test model", "Serial 1").unwrap(),
                ..selected.clone()
            },
            Pkcs11Selection {
                certificate_sha256: [0; 32],
                ..selected.clone()
            },
            Pkcs11Selection {
                key_id: vec![1, 3],
                ..selected.clone()
            },
        ] {
            assert_ne!(original, changed.binding());
        }
        assert!(!format!("{selected:?}").contains("synthetic"));
        let signer = Pkcs11Signer::new(selected, pin_policy(), Duration::from_secs(30)).unwrap();
        assert!(!format!("{signer:?}").contains("synthetic-secret-pin"));
        assert!(!format!("{:?}", pin_policy()).contains("synthetic-secret-pin"));
        assert_eq!(signer.certificate_sha256(), [0xcd; 32]);
    }

    #[test]
    fn token_fields_trim_only_pkcs11_space_padding_and_match_all_components() {
        let selected = Pkcs11Token::new("Vendor  ", "Model ", "Serial ").unwrap();
        assert!(selected.matches("Vendor ", "Model  ", "Serial  "));
        for (manufacturer, model, serial) in [
            ("Other", "Model", "Serial"),
            ("Vendor", "Other", "Serial"),
            ("Vendor", "Model", "Other"),
            ("vendor", "Model", "Serial"),
            (" Vendor", "Model", "Serial"),
        ] {
            assert!(!selected.matches(manufacturer, model, serial));
        }
        for (manufacturer, model, serial, field) in [
            ("", "Model", "Serial", Pkcs11Field::Manufacturer),
            ("Vendor", "", "Serial", Pkcs11Field::Model),
            ("Vendor", "Model", " ", Pkcs11Field::Serial),
            ("Vendor\n", "Model", "Serial", Pkcs11Field::Manufacturer),
            (
                "Vendor\u{fffd}",
                "Model",
                "Serial",
                Pkcs11Field::Manufacturer,
            ),
            ("Vendor", "Model", "Serial\0", Pkcs11Field::Serial),
        ] {
            assert_eq!(
                Pkcs11Token::new(manufacturer, model, serial)
                    .unwrap_err()
                    .field,
                field
            );
        }
        assert!(Pkcs11Token::new(&"a".repeat(33), "M", "S").is_err());
        assert!(Pkcs11Token::new("V", &"a".repeat(17), "S").is_err());
        assert!(Pkcs11Token::new("V", "M", &"a".repeat(17)).is_err());
    }

    #[test]
    fn module_path_custody_and_hash_contract_are_explicit_and_bounded() {
        for path in [
            "libpkcs11.so",
            "../libpkcs11.so",
            "/opt/../libpkcs11.so",
            "/opt/./libpkcs11.so",
            "/opt//libpkcs11.so",
            "/opt/p11-kit-proxy.so",
            "/opt/renamed-proxy.so",
            "/opt/bad\nname.so",
        ] {
            assert_eq!(
                validate_module_path(Path::new(path)).unwrap_err().field,
                Pkcs11Field::Module
            );
        }
        assert!(root_custody(0, 0o100644));
        assert!(!root_custody(1000, 0o100444));
        assert!(!root_custody(0, 0o100664));
        assert!(!root_custody(0, 0o100646));
        let expected: [u8; 32] = digest(&SHA256, b"synthetic module")
            .as_ref()
            .try_into()
            .unwrap();
        let mut reader = Cursor::new(b"synthetic module");
        assert_eq!(
            hash_module_reader(&mut reader, &control(), 16).unwrap(),
            expected
        );
        assert!(hash_module_reader(&mut reader, &control(), 15).is_err());
        assert_ne!(
            hash_module_reader(&mut Cursor::new(b"different module"), &control(), 16).unwrap(),
            expected
        );
        let stopped = control();
        stopped.cancelled.store(true, Ordering::Release);
        assert_eq!(
            hash_module_reader(&mut reader, &stopped, 16),
            Err(failure(CertificateError::Cancelled))
        );
        let stamp = ModuleStamp {
            device: 1,
            inode: 2,
            size: 3,
            modified: (4, 5),
            changed: (6, 7),
            uid: 0,
            mode: 0o100644,
            links: 1,
        };
        assert_ne!(stamp, ModuleStamp { inode: 3, ..stamp });
        assert_ne!(
            stamp,
            ModuleStamp {
                changed: (6, 8),
                ..stamp
            }
        );
    }

    #[test]
    fn deadlines_key_ids_and_pin_policy_do_not_have_implicit_defaults() {
        assert!(
            Pkcs11Selection::new(
                PathBuf::from("/opt/module.so"),
                [0; 32],
                0,
                token(),
                [0; 32],
                vec![],
            )
            .is_err()
        );
        assert!(
            Pkcs11Selection::new(
                PathBuf::from("/opt/module.so"),
                [0; 32],
                0,
                token(),
                [0; 32],
                vec![1; KEY_ID_LIMIT + 1],
            )
            .is_err()
        );
        assert!(Pkcs11Signer::new(selection(), pin_policy(), Duration::ZERO).is_err());
        assert!(
            Pkcs11Signer::new(selection(), pin_policy(), MAX_WAIT + Duration::from_secs(1))
                .is_err()
        );
        assert!(validate_authentication(&pin_policy(), false).is_ok());
        assert!(validate_authentication(&pin_policy(), true).is_err());
        assert!(validate_authentication(&Pkcs11Authentication::ProtectedPath, false).is_err());
        assert!(validate_authentication(&Pkcs11Authentication::ProtectedPath, true).is_ok());
        let absent: Arc<dyn Pkcs11PinSource> =
            Arc::new(|| Err(failure(CertificateError::PinUnavailable)));
        assert_eq!(
            absent.pin().unwrap_err(),
            failure(CertificateError::PinUnavailable)
        );
    }

    fn hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    // Test-only DER envelope around the existing licensed public Wycheproof key.
    // Its outer certificate signature is deliberately synthetic, not a CA proof.
    fn der(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut result = vec![tag];
        if value.len() < 128 {
            result.push(value.len() as u8);
        } else if value.len() < 256 {
            result.extend_from_slice(&[0x81, value.len() as u8]);
        } else {
            result.extend_from_slice(&[0x82, (value.len() >> 8) as u8, value.len() as u8]);
        }
        result.extend_from_slice(value);
        result
    }

    fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
        der(0x30, &parts.concat())
    }

    fn public_certificate() -> Vec<u8> {
        let rsa_algorithm = sequence(&[
            der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1]),
            der(0x05, &[]),
        ]);
        let signature_algorithm = sequence(&[
            der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 11]),
            der(0x05, &[]),
        ]);
        let mut modulus = vec![0];
        modulus.extend_from_slice(&hex(public_vectors::MODULUS));
        let key = sequence(&[
            der(0x02, &modulus),
            der(0x02, &hex(public_vectors::EXPONENT)),
        ]);
        let mut public_bits = vec![0];
        public_bits.extend_from_slice(&key);
        let tbs = sequence(&[
            der(0xa0, &der(0x02, &[2])),
            der(0x02, &[1]),
            signature_algorithm.clone(),
            sequence(&[]),
            sequence(&[der(0x17, b"200101000000Z"), der(0x17, b"491231235959Z")]),
            sequence(&[]),
            sequence(&[rsa_algorithm, der(0x03, &public_bits)]),
        ]);
        sequence(&[tbs, signature_algorithm, der(0x03, &[0, 0])])
    }

    fn certificate() -> CertificatePublicKey {
        let der = public_certificate();
        let fingerprint = digest(&SHA256, &der).as_ref().try_into().unwrap();
        CertificatePublicKey::parse(&der, &fingerprint).unwrap()
    }

    fn key_attributes(certificate: &CertificatePublicKey) -> Vec<Attribute> {
        vec![
            Attribute::Class(ObjectClass::PRIVATE_KEY),
            Attribute::KeyType(KeyType::RSA),
            Attribute::Id(vec![1, 2]),
            Attribute::Token(true),
            Attribute::Private(true),
            Attribute::Sign(true),
            Attribute::Extractable(false),
            Attribute::NeverExtractable(true),
            Attribute::Sensitive(true),
            Attribute::AlwaysSensitive(true),
            Attribute::AlwaysAuthenticate(false),
            Attribute::Modulus(certificate.modulus.clone()),
            Attribute::PublicExponent(certificate.exponent.clone()),
        ]
    }

    #[test]
    fn required_key_attributes_must_all_be_present_unique_and_satisfy_policy() {
        let certificate = certificate();
        let attributes = key_attributes(&certificate);
        assert!(validate_key_attributes(&attributes, &[1, 2], &certificate).is_ok());
        for index in 0..attributes.len() {
            let mut missing = attributes.clone();
            missing.remove(index);
            assert!(validate_key_attributes(&missing, &[1, 2], &certificate).is_err());
            let mut duplicate = attributes.clone();
            duplicate[index] = attributes[(index + 1) % attributes.len()].clone();
            assert!(validate_key_attributes(&duplicate, &[1, 2], &certificate).is_err());
        }
        for rejected in [
            Attribute::Class(ObjectClass::PUBLIC_KEY),
            Attribute::KeyType(KeyType::EC),
            Attribute::Id(vec![1, 3]),
            Attribute::Token(false),
            Attribute::Private(false),
            Attribute::Sign(false),
            Attribute::Extractable(true),
            Attribute::NeverExtractable(false),
            Attribute::Sensitive(false),
            Attribute::AlwaysSensitive(false),
            Attribute::AlwaysAuthenticate(true),
            Attribute::Modulus(vec![1; 256]),
            Attribute::PublicExponent(vec![3]),
        ] {
            let mut changed = attributes.clone();
            let index = changed
                .iter()
                .position(|value| value.attribute_type() == rejected.attribute_type())
                .unwrap();
            changed[index] = rejected;
            assert!(validate_key_attributes(&changed, &[1, 2], &certificate).is_err());
        }
        assert!(validate_key_attributes(&attributes, &[1, 3], &certificate).is_err());
        assert!(unique_object::<u32>(&[]).is_err());
        assert!(unique_object(&[7, 7]).is_err());
        assert_eq!(*unique_object(&[7]).unwrap(), 7);
    }

    #[test]
    fn certificate_fingerprint_id_and_duplicate_der_objects_are_not_interchangeable() {
        let der = public_certificate();
        let fingerprint: [u8; 32] = digest(&SHA256, &der).as_ref().try_into().unwrap();
        assert!(CertificatePublicKey::parse(&der, &[0; 32]).is_err());
        assert!(CertificatePublicKey::parse(b"not DER", &[0; 32]).is_err());
        let mut trailing = der.clone();
        trailing.push(0);
        let trailing_hash = digest(&SHA256, &trailing).as_ref().try_into().unwrap();
        assert!(CertificatePublicKey::parse(&trailing, &trailing_hash).is_err());
        let mut matched = None;
        record_certificate_match(&mut matched, &der, &[0; 32]).unwrap();
        assert!(matched.is_none());
        record_certificate_match(&mut matched, &der, &fingerprint).unwrap();
        assert!(matched.is_some());
        assert!(record_certificate_match(&mut matched, &der, &fingerprint).is_err());
        let attributes = [
            Attribute::Class(ObjectClass::CERTIFICATE),
            Attribute::CertificateType(CertificateType::X_509),
            Attribute::Token(true),
            Attribute::Id(vec![1, 2]),
            Attribute::Value(der.clone()),
        ];
        assert_eq!(certificate_der(&attributes, &[1, 2]).unwrap(), der);
        assert!(certificate_der(&attributes, &[1, 3]).is_err());
    }

    #[test]
    fn ps256_uses_the_selected_certificate_key_and_exact_message_or_digest_contract() {
        let certificate = certificate();
        let signature = hex(public_vectors::SIGNATURE);
        assert_eq!(certificate.bits, 2048);
        certificate
            .verify(public_vectors::MESSAGE, &signature)
            .unwrap();
        assert!(certificate.verify(b"different", &signature).is_err());
        let mut changed = signature.clone();
        changed[0] ^= 1;
        assert!(
            certificate
                .verify(public_vectors::MESSAGE, &changed)
                .is_err()
        );
        let unrelated = CertificatePublicKey {
            modulus: vec![0xff; 256],
            exponent: certificate.exponent.clone(),
            bits: 2048,
            validity: certificate.validity.clone(),
        };
        assert!(
            unrelated
                .verify(public_vectors::MESSAGE, &signature)
                .is_err()
        );
        assert_eq!(SigningPlan::Message.input(b"Test"), b"Test");
        assert_eq!(
            SigningPlan::Digest.input(b"Test"),
            digest(&SHA256, b"Test").as_ref()
        );
        assert!(
            certificate
                .verify(&SigningPlan::Digest.input(b"Test"), &signature)
                .is_err()
        );
        for plan in [SigningPlan::Message, SigningPlan::Digest] {
            let parameters = match plan.mechanism() {
                Mechanism::Sha256RsaPkcsPss(parameters) if plan == SigningPlan::Message => {
                    parameters
                }
                Mechanism::RsaPkcsPss(parameters) if plan == SigningPlan::Digest => parameters,
                _ => panic!("wrong message/digest mechanism"),
            };
            assert_eq!(parameters.hash_alg, MechanismType::SHA256);
            assert_eq!(parameters.mgf, PkcsMgfType::MGF1_SHA256);
            assert_eq!(*parameters.s_len, 32);
        }
    }

    struct FakeForeignCall {
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<bool>,
        changed: Condvar,
        finished: AtomicBool,
    }

    impl FakeForeignCall {
        fn new() -> (Arc<Self>, tokio::sync::oneshot::Receiver<()>) {
            let (send, receive) = tokio::sync::oneshot::channel();
            (
                Arc::new(Self {
                    entered: Mutex::new(Some(send)),
                    release: Mutex::new(false),
                    changed: Condvar::new(),
                    finished: AtomicBool::new(false),
                }),
                receive,
            )
        }

        fn run(&self, control: &Control) -> Result<Vec<u8>, HeadlessError> {
            self.entered
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            let mut release = self.release.lock().unwrap();
            while !*release {
                // A finite test-only watchdog prevents a failed assertion from
                // leaving the test runtime blocked indefinitely.
                let result = self
                    .changed
                    .wait_timeout(release, Duration::from_secs(5))
                    .unwrap();
                release = result.0;
                if result.1.timed_out() {
                    break;
                }
            }
            self.finished.store(true, Ordering::Release);
            control.checkpoint()?;
            Ok(vec![0; 256])
        }

        fn release(&self) {
            *self.release.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }

    async fn capacity_is_released(pool: &Arc<Semaphore>) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while pool.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn timeout_retains_capacity_until_foreign_work_and_cleanup_really_finish() {
        let pool = Arc::new(Semaphore::new(1));
        let (foreign, entered) = FakeForeignCall::new();
        let operation = foreign.clone();
        let capacity = pool.clone();
        let waiting = tokio::spawn(async move {
            run_worker(
                capacity,
                Instant::now() + Duration::from_millis(500),
                move |control| operation.run(control),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            waiting.await.unwrap(),
            Err(failure(CertificateError::CleanupPending))
        );
        assert!(!foreign.finished.load(Ordering::Acquire));
        assert_eq!(pool.available_permits(), 0);
        assert_eq!(
            run_worker(
                pool.clone(),
                Instant::now() + Duration::from_secs(1),
                |_| Ok(vec![])
            )
            .await,
            Err(failure(CertificateError::Capacity))
        );
        foreign.release();
        capacity_is_released(&pool).await;
        assert!(foreign.finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn dropped_waiter_cancels_publication_but_does_not_release_foreign_capacity() {
        let pool = Arc::new(Semaphore::new(1));
        let (foreign, entered) = FakeForeignCall::new();
        let operation = foreign.clone();
        let capacity = pool.clone();
        let waiting = tokio::spawn(async move {
            run_worker(
                capacity,
                Instant::now() + Duration::from_secs(30),
                move |control| operation.run(control),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        assert_eq!(pool.available_permits(), 0);
        assert!(!foreign.finished.load(Ordering::Acquire));
        foreign.release();
        capacity_is_released(&pool).await;
        assert!(foreign.finished.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn unknown_cleanup_and_panics_quarantine_rather_than_recycle_capacity() {
        for panic in [false, true] {
            let pool = Arc::new(Semaphore::new(1));
            let result = run_worker(
                pool.clone(),
                Instant::now() + Duration::from_secs(1),
                move |_| {
                    assert!(!panic, "synthetic worker panic");
                    Err(failure(CertificateError::CleanupPending))
                },
            )
            .await;
            assert_eq!(result, Err(failure(CertificateError::CleanupPending)));
            assert_eq!(pool.available_permits(), 0);
        }
    }

    #[test]
    fn searches_have_a_complete_bound_and_propagate_unknown_failures() {
        let accepted = bounded_collect((0..OBJECT_LIMIT).map(Ok), &control()).unwrap();
        assert_eq!(accepted.len(), OBJECT_LIMIT);
        assert_eq!(
            bounded_collect((0..=OBJECT_LIMIT).map(Ok), &control()),
            Err(failure(CertificateError::SearchLimit))
        );
        assert_eq!(
            bounded_collect(
                std::iter::once(Err::<u32, _>(failure(CertificateError::Provider))),
                &control()
            ),
            Err(failure(CertificateError::Provider))
        );
    }

    #[tokio::test]
    async fn expired_absolute_parent_deadline_never_reserves_or_starts_foreign_work() {
        let pool = Arc::new(Semaphore::new(1));
        assert_eq!(
            run_worker(pool.clone(), Instant::now(), |_| {
                panic!("expired work must not start")
            })
            .await,
            Err(failure(CertificateError::Cancelled))
        );
        assert_eq!(pool.available_permits(), 1);
        let signer = Pkcs11Signer::new(selection(), pin_policy(), Duration::from_secs(30))
            .unwrap()
            .with_parent_deadline(Instant::now());
        assert_eq!(
            signer.sign_ps256(b"synthetic-input").await,
            Err(failure(CertificateError::Cancelled))
        );
    }
}
