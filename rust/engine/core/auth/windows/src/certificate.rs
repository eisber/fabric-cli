// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Safe certificate-signing facade with capacity independent of WAM.

use std::{
    fmt,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};
use thiserror::Error;

const WORKER_LIMIT: usize = 2;
const SIGNING_INPUT_LIMIT: usize = 8192;

/// One explicit local personal-certificate store, never an ambient search list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateStore {
    /// The current process identity's personal-certificate store.
    CurrentUserMy,
    /// The local machine's personal-certificate store.
    LocalMachineMy,
}

/// Allowlist applied to the provider associated with the acquired key.
///
/// This policy never selects, opens, repairs or replaces a key provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllowedCertificateProvider {
    /// Require the associated Microsoft Software Key Storage Provider.
    MicrosoftSoftware,
}

/// Explicit store, public-certificate fingerprint and associated-provider policy.
#[derive(Clone, Eq, PartialEq)]
pub struct CertificateSelector {
    store: CertificateStore,
    sha256: [u8; 32],
    allowed_provider: AllowedCertificateProvider,
}

impl CertificateSelector {
    /// Captures a selection without enumerating certificates or acquiring a key.
    pub fn new(
        store: CertificateStore,
        sha256: [u8; 32],
        allowed_provider: AllowedCertificateProvider,
    ) -> Self {
        Self {
            store,
            sha256,
            allowed_provider,
        }
    }

    /// Returns the explicitly selected public certificate's SHA-256 fingerprint.
    pub fn certificate_sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// Returns non-secret selection metadata for identity-bound batch isolation.
    ///
    /// A changed fingerprint requires a new selection, not implicit rollover.
    pub fn binding(&self) -> String {
        use std::fmt::Write as _;

        let store = match self.store {
            CertificateStore::CurrentUserMy => "current-user-my",
            CertificateStore::LocalMachineMy => "local-machine-my",
        };
        let mut binding = format!("windows-certificate-store:{store}:microsoft-software:");
        for byte in self.sha256 {
            let _ = write!(binding, "{byte:02x}");
        }
        binding
    }

    pub(crate) fn store(&self) -> CertificateStore {
        self.store
    }

    pub(crate) fn allowed_provider(&self) -> AllowedCertificateProvider {
        self.allowed_provider
    }
}

impl fmt::Debug for CertificateSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertificateSelector")
            .finish_non_exhaustive()
    }
}

pub(crate) fn validate_key_policy(
    allowed_provider: AllowedCertificateProvider,
    actual_provider: &str,
    export_policy: Option<u32>,
    can_sign: bool,
    is_rsa: bool,
    bits: u32,
) -> Result<(), CertificateError> {
    let expected = match allowed_provider {
        AllowedCertificateProvider::MicrosoftSoftware => "Microsoft Software Key Storage Provider",
    };
    if actual_provider != expected {
        return Err(CertificateError::ProviderNotAllowed);
    }
    match export_policy {
        Some(0) => {}
        Some(_) => return Err(CertificateError::ExportableKey),
        None => return Err(CertificateError::KeyPolicyUnavailable),
    }
    if !can_sign || !is_rsa || !(2048..=4096).contains(&bits) {
        return Err(CertificateError::UnsupportedKey);
    }
    Ok(())
}

/// Native certificate stages, without subject names, key names or provider text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateStage {
    /// Open the explicitly selected existing store.
    OpenStore,
    /// Read bounded public-certificate metadata.
    ReadCertificate,
    /// Acquire the key already associated with the retained certificate.
    AcquireKey,
    /// Inspect the acquired key's actual associated provider.
    ReadProvider,
    /// Inspect signing, algorithm and non-exportability properties.
    ReadKeyPolicy,
    /// Compute SHA-256 through BCrypt.
    Hash,
    /// Sign through NCrypt without UI.
    Sign,
    /// Import only the retained certificate's public key.
    ImportPublicKey,
    /// Independently verify the resulting signature.
    VerifySignature,
    /// Obtain an assertion nonce through the system BCrypt random generator.
    Nonce,
    /// Release a caller-owned private-key handle.
    ReleaseKey,
    /// Release a caller-owned provider handle.
    ReleaseProvider,
    /// Release an imported public-key handle.
    ReleasePublicKey,
    /// Release the retained certificate context.
    ReleaseCertificate,
    /// Close the opened store.
    CloseStore,
}

/// Bounded certificate failures without secret material or native error strings.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CertificateError {
    /// The signing input exceeds the explicit contract.
    #[error("invalid certificate signing input")]
    InvalidInput,
    /// All certificate-only native slots are occupied or quarantined.
    #[error("certificate signing capacity exhausted ({active}/{limit})")]
    CapacityExhausted {
        /// Occupied or quarantined certificate-only slots.
        active: usize,
        /// Fixed certificate-only capacity, independent of WAM.
        limit: usize,
    },
    /// Waiting was cancelled; native work may still own its slot.
    #[error("certificate signing was cancelled")]
    Cancelled,
    /// The explicit certificate was not found.
    #[error("the selected certificate was not found")]
    NotFound,
    /// The selected store contains more than one matching context.
    #[error("the certificate selection is ambiguous")]
    AmbiguousMatch,
    /// A complete bounded certificate search could not be established.
    #[error("the certificate search exceeded its bounds")]
    SearchLimit,
    /// The acquired key's actual provider is not allowed.
    #[error("the associated certificate key provider is not allowed")]
    ProviderNotAllowed,
    /// Required key policy could not be established.
    #[error("the certificate key policy could not be established")]
    KeyPolicyUnavailable,
    /// The associated key permits export or archival.
    #[error("the certificate key does not satisfy the non-exportability policy")]
    ExportableKey,
    /// The key does not support the required RSA/PSS contract.
    #[error("the certificate key does not support the required signing contract")]
    UnsupportedKey,
    /// The selected key requires user interaction, which this API never permits.
    #[error("the selected certificate key requires user interaction")]
    InteractionRequired,
    /// Certificate metadata is malformed, expired or not yet valid.
    #[error("the selected certificate has invalid metadata")]
    CertificateMetadata,
    /// The signature does not verify with the retained certificate's public key.
    #[error("the certificate signature could not be verified")]
    SignatureInvalid,
    /// A certificate-only worker could not be created.
    #[error("the certificate signing worker could not be started")]
    WorkerStart,
    /// A worker panicked; its slot is not silently recycled.
    #[error("the certificate signing worker failed")]
    WorkerFailed,
    /// Native cleanup could not be confirmed; its slot remains occupied.
    #[error("certificate cleanup is unconfirmed at {stage:?} (code {code:#010x})")]
    CleanupUnconfirmed {
        /// Cleanup stage, never provider text.
        stage: CertificateStage,
        /// Numeric native status.
        code: i32,
    },
    /// A native call failed without retaining a provider error message.
    #[error("certificate operation failed at {stage:?} (code {code:#010x})")]
    Native {
        /// Native stage.
        stage: CertificateStage,
        /// Numeric native status.
        code: i32,
    },
}

struct WorkerPool {
    active: AtomicUsize,
    limit: usize,
}

impl WorkerPool {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            limit,
        })
    }

    fn reserve(self: &Arc<Self>) -> Result<WorkerPermit, CertificateError> {
        self.active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .map(|_| WorkerPermit {
                pool: Arc::clone(self),
                release: true,
            })
            .map_err(|active| CertificateError::CapacityExhausted {
                active,
                limit: self.limit,
            })
    }
}

struct WorkerPermit {
    pool: Arc<WorkerPool>,
    release: bool,
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        if self.release {
            self.pool.active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

fn worker_pool() -> &'static Arc<WorkerPool> {
    static POOL: OnceLock<Arc<WorkerPool>> = OnceLock::new();
    POOL.get_or_init(|| WorkerPool::new(WORKER_LIMIT))
}

struct State {
    result: Option<Result<Vec<u8>, CertificateError>>,
    delivered: bool,
    waker: Option<Waker>,
}

struct Shared {
    state: Mutex<State>,
    cancelled: AtomicBool,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                result: None,
                delivered: false,
                waker: None,
            }),
            cancelled: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| {
            self.cancelled.store(true, Ordering::Release);
            poisoned.into_inner()
        })
    }

    fn cancel(&self) {
        let waker = {
            let mut state = self.lock();
            if state.delivered {
                return;
            }
            self.cancelled.store(true, Ordering::Release);
            state.result = Some(Err(CertificateError::Cancelled));
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn complete(&self, result: Result<Vec<u8>, CertificateError>) {
        let waker = {
            let mut state = self.lock();
            if state.delivered {
                return;
            }
            state.result = Some(if self.is_cancelled() {
                Err(CertificateError::Cancelled)
            } else {
                result
            });
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Result and teardown evidence returned only after native resource cleanup.
pub(crate) struct WorkOutcome {
    pub(crate) result: Result<Vec<u8>, CertificateError>,
    pub(crate) cleanup_confirmed: bool,
}

/// Awaitable certificate-only work; dropping it invalidates late publication.
///
/// Cancellation does not interrupt a synchronous CNG call or recycle its slot.
/// The worker owns all native resources and its permit until actual teardown.
pub struct CertificateSignOperation {
    shared: Arc<Shared>,
}

impl CertificateSignOperation {
    /// Cancels local waiting without claiming native rollback or completed cleanup.
    pub fn cancel(&self) {
        self.shared.cancel();
    }

    /// Returns certificate-only occupied/quarantined slots and their fixed limit.
    pub fn worker_counts() -> (usize, usize) {
        let pool = worker_pool();
        (pool.active.load(Ordering::Acquire), pool.limit)
    }
}

impl fmt::Debug for CertificateSignOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertificateSignOperation")
            .finish_non_exhaustive()
    }
}

impl Future for CertificateSignOperation {
    type Output = Result<Vec<u8>, CertificateError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.shared.lock();
        if state.delivered {
            return Poll::Ready(Err(CertificateError::Cancelled));
        }
        if self.shared.is_cancelled() {
            state.delivered = true;
            state.result = None;
            return Poll::Ready(Err(CertificateError::Cancelled));
        }
        match state.result.take() {
            Some(result) => {
                state.delivered = true;
                Poll::Ready(result)
            }
            None => {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for CertificateSignOperation {
    fn drop(&mut self) {
        self.shared.cancel();
    }
}

fn start_with(
    selector: CertificateSelector,
    signing_input: Vec<u8>,
    pool: Arc<WorkerPool>,
    work: impl FnOnce(&CertificateSelector, &[u8], &dyn Fn() -> bool) -> WorkOutcome + Send + 'static,
) -> Result<CertificateSignOperation, CertificateError> {
    if signing_input.is_empty() || signing_input.len() > SIGNING_INPUT_LIMIT {
        return Err(CertificateError::InvalidInput);
    }
    let mut permit = pool.reserve()?;
    let shared = Shared::new();
    let worker_shared = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("fabric-certificate".into())
        .spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                if worker_shared.is_cancelled() {
                    return WorkOutcome {
                        result: Err(CertificateError::Cancelled),
                        cleanup_confirmed: true,
                    };
                }
                work(&selector, &signing_input, &|| worker_shared.is_cancelled())
            }));
            let result = match outcome {
                Ok(outcome) => {
                    permit.release = outcome.cleanup_confirmed;
                    if !outcome.cleanup_confirmed && outcome.result.is_ok() {
                        Err(CertificateError::WorkerFailed)
                    } else {
                        outcome.result
                    }
                }
                Err(_) => {
                    permit.release = false;
                    Err(CertificateError::WorkerFailed)
                }
            };
            worker_shared.complete(result);
            drop(permit);
        })
        .map_err(|_| CertificateError::WorkerStart)?;
    Ok(CertificateSignOperation { shared })
}

/// Starts bounded native RSA/PSS/SHA-256 signing for an explicit certificate.
///
/// The native task retains one matched certificate context through acquisition,
/// associated-provider checks, signing, public-key verification and cleanup.
/// No key export, provider rebinding, implicit store or interactive UI is allowed.
///
/// # Errors
/// Rejects invalid input or exhausted certificate capacity before creating a
/// worker. Native selection/policy/signing errors are returned through the future.
pub fn sign_ps256(
    selector: CertificateSelector,
    signing_input: Vec<u8>,
) -> Result<CertificateSignOperation, CertificateError> {
    start_with(
        selector,
        signing_input,
        Arc::clone(worker_pool()),
        crate::native::certificate::sign_ps256,
    )
}

/// Obtains a fresh 128-bit nonce from the system BCrypt random generator.
///
/// This function performs no store discovery, key access or signing.
///
/// # Errors
/// Returns only a numeric native error if system randomness is unavailable.
pub fn assertion_nonce() -> Result<[u8; 16], CertificateError> {
    crate::native::certificate::assertion_nonce()
}

#[cfg(test)]
#[path = "certificate_tests.rs"]
mod tests;
