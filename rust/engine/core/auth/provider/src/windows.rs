// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::{Error, ErrorKind},
    time::OffsetDateTime,
};
use fabric_identity_windows::{self as wam, BrokerError, EntraOptions, Interaction, TokenAudience};
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::Duration,
};

/// Explicit public-cloud account/tenant/client configuration.
pub use wam::EntraOptions as LoginOptions;

const IDENTITY_LIMIT: usize = 64;
const DRAIN_LIMIT: Duration = Duration::from_secs(5);
const SILENT_LIMIT: Duration = Duration::from_secs(120);
const INTERACTIVE_LIMIT: Duration = Duration::from_secs(600);

/// Non-secret token metadata safe to report after successful acquisition.
#[derive(Clone, Copy, Debug)]
pub struct TokenMetadata {
    /// Exact requested scope.
    pub scope: &'static str,
    /// Broker-derived expiry in Unix seconds.
    pub expires_on: i64,
}

#[async_trait]
trait Cleanup: Send + Sync {
    fn cancel(&self);
    fn ended(&self) -> bool;
    fn confirmed(&self) -> bool;
    async fn wait(&self) -> Result<(), BrokerError>;
}

#[async_trait]
impl Cleanup for wam::OperationControl {
    fn cancel(&self) {
        self.cancel();
    }

    fn ended(&self) -> bool {
        self.has_ended()
    }

    fn confirmed(&self) -> bool {
        self.cleanup_confirmed()
    }

    async fn wait(&self) -> Result<(), BrokerError> {
        while !self.has_ended() {
            tokio::time::sleep(Duration::from_millis(16)).await;
        }
        if self.cleanup_confirmed() {
            Ok(())
        } else {
            Err(cleanup_pending())
        }
    }
}

type BrokerFuture<T> = Pin<Box<dyn Future<Output = Result<T, BrokerError>> + Send>>;

struct Running<T> {
    control: Arc<dyn Cleanup>,
    result: BrokerFuture<T>,
}

struct Session {
    account_id: String,
    tokens: Vec<SessionToken>,
}

struct SessionToken {
    audience: TokenAudience,
    expires_on: i64,
    secret: String,
}

trait Broker: Send + Sync {
    fn acquire(
        &self,
        options: EntraOptions,
        binding: Option<String>,
        audiences: Vec<TokenAudience>,
        interaction: Interaction,
    ) -> Running<Session>;

    fn logout(&self, options: EntraOptions, binding: Option<String>) -> Running<Option<String>>;
}

struct NativeBroker;

impl Broker for NativeBroker {
    fn acquire(
        &self,
        options: EntraOptions,
        binding: Option<String>,
        audiences: Vec<TokenAudience>,
        interaction: Interaction,
    ) -> Running<Session> {
        let operation = wam::acquire(options, binding, audiences, interaction);
        Running {
            control: Arc::new(operation.control()),
            result: Box::pin(async move {
                let session = operation.await?;
                let account_id = session.account_id().to_owned();
                let tokens = session
                    .into_tokens()
                    .into_iter()
                    .map(|token| SessionToken {
                        audience: token.audience(),
                        expires_on: token.expires_on(),
                        secret: token.into_secret(),
                    })
                    .collect();
                Ok(Session { account_id, tokens })
            }),
        }
    }

    fn logout(&self, options: EntraOptions, binding: Option<String>) -> Running<Option<String>> {
        let operation = wam::logout_bound(options, binding);
        Running {
            control: Arc::new(operation.control()),
            result: Box::pin(operation),
        }
    }
}

#[derive(Default)]
struct SlotState {
    cancelled: bool,
    abandoned: bool,
    control: Option<Arc<dyn Cleanup>>,
}

#[derive(Default)]
struct Slot {
    state: Mutex<SlotState>,
    attached: tokio::sync::Notify,
}

impl Slot {
    fn attach(&self, control: Arc<dyn Cleanup>) -> Result<(), BrokerError> {
        let cancelled = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| BrokerError::CoordinationFailed)?;
            state.control = Some(Arc::clone(&control));
            state.cancelled
        };
        if cancelled {
            control.cancel();
        }
        self.attached.notify_waiters();
        Ok(())
    }

    fn cancel(&self) {
        self.cancel_inner(false);
    }

    fn abandon(&self) {
        self.cancel_inner(true);
    }

    fn cancel_inner(&self, abandoned: bool) {
        let control = match self.state.lock() {
            Ok(mut state) => {
                state.cancelled = true;
                state.abandoned |= abandoned;
                state.control.clone()
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.cancelled = true;
                state.abandoned |= abandoned;
                state.control.clone()
            }
        };
        if let Some(control) = control {
            control.cancel();
        }
        self.attached.notify_waiters();
    }

    fn finished(&self) -> Result<bool, BrokerError> {
        let state = self
            .state
            .lock()
            .map_err(|_| BrokerError::CoordinationFailed)?;
        match &state.control {
            Some(control) if control.ended() && !control.confirmed() => Err(cleanup_pending()),
            Some(control) => Ok(control.ended()),
            None => Ok(state.abandoned),
        }
    }

    async fn wait(&self) -> Result<(), BrokerError> {
        loop {
            let attached = self.attached.notified();
            tokio::pin!(attached);
            attached.as_mut().enable();
            let (control, abandoned) = {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| BrokerError::CoordinationFailed)?;
                (state.control.clone(), state.abandoned)
            };
            if let Some(control) = control {
                return control.wait().await;
            }
            if abandoned {
                return Ok(());
            }
            attached.await;
        }
    }
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Phase {
    Fresh,
    Active,
    SignedOut,
    Unconfirmed,
    SigningOut,
}

struct Domain {
    parent: usize,
    invalidated: u64,
    phase: Phase,
    unresolved: bool,
    slots: Vec<Arc<Slot>>,
    logout_slots: Vec<Arc<Slot>>,
}

struct Entry {
    tenant: String,
    client: String,
    account: String,
    domain: usize,
    binding: Option<String>,
}

#[derive(Default)]
struct Registry {
    clock: u64,
    failed: bool,
    entries: Vec<Entry>,
    domains: Vec<Domain>,
    native_invalidations: BTreeMap<(String, String), u64>,
    client_invalidations: BTreeMap<String, u64>,
}

impl Registry {
    fn tick(&mut self) -> Result<u64, BrokerError> {
        let Some(next) = self.clock.checked_add(1) else {
            self.failed = true;
            return Err(BrokerError::CoordinationFailed);
        };
        self.clock = next;
        Ok(self.clock)
    }

    fn root(&self, mut domain: usize) -> usize {
        while self.domains[domain].parent != domain {
            domain = self.domains[domain].parent;
        }
        domain
    }

    fn entry(&mut self, options: &EntraOptions) -> Result<usize, BrokerError> {
        let account = options.account().to_ascii_lowercase();
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.tenant == options.tenant()
                && entry.client == options.client()
                && entry.account == account
        }) {
            return Ok(index);
        }
        if self.entries.len() >= IDENTITY_LIMIT {
            return Err(BrokerError::IdentityCapacityExhausted {
                limit: IDENTITY_LIMIT,
            });
        }
        // Tenant keys join before ANY native acquisition or returned identity.
        let domain = if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.client == options.client() && entry.account == account)
        {
            self.root(entry.domain)
        } else {
            let index = self.domains.len();
            self.domains.push(Domain {
                parent: index,
                invalidated: 0,
                phase: Phase::Fresh,
                unresolved: false,
                slots: Vec::new(),
                logout_slots: Vec::new(),
            });
            index
        };
        let index = self.entries.len();
        self.entries.push(Entry {
            tenant: options.tenant().into(),
            client: options.client().into(),
            account,
            domain,
            binding: None,
        });
        Ok(index)
    }

    fn merge(&mut self, left: usize, right: usize) -> usize {
        let left = self.root(left);
        let right = self.root(right);
        if left == right {
            return left;
        }
        let (keep, old) = (left.min(right), left.max(right));
        let slots = std::mem::take(&mut self.domains[old].slots);
        self.domains[keep].slots.extend(slots);
        let logout_slots = std::mem::take(&mut self.domains[old].logout_slots);
        self.domains[keep].logout_slots.extend(logout_slots);
        self.domains[keep].invalidated = self.domains[keep]
            .invalidated
            .max(self.domains[old].invalidated);
        self.domains[keep].phase = self.domains[keep].phase.max(self.domains[old].phase);
        let unresolved = self.domains[old].unresolved;
        self.domains[keep].unresolved |= unresolved;
        self.domains[old].parent = keep;
        keep
    }

    fn bind(&mut self, index: usize, native_id: &str) -> Result<usize, BrokerError> {
        let domain = self.root(self.entries[index].domain);
        if native_id.is_empty()
            || self.entries[index]
                .binding
                .as_deref()
                .is_some_and(|expected| expected != native_id)
            || self.entries.iter().any(|entry| {
                self.root(entry.domain) == domain
                    && entry
                        .binding
                        .as_deref()
                        .is_some_and(|expected| expected != native_id)
            })
        {
            return Err(BrokerError::IdentityMismatch);
        }
        self.entries[index].binding = Some(native_id.into());
        let client = self.entries[index].client.clone();
        let mut root = self.root(self.entries[index].domain);
        let matching: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.client == client && entry.binding.as_deref() == Some(native_id))
            .map(|entry| entry.domain)
            .collect();
        for domain in matching {
            root = self.merge(root, domain);
        }
        let cutoff = self
            .native_invalidations
            .get(&(client.clone(), native_id.to_owned()))
            .copied()
            .unwrap_or(0)
            .max(self.domains[root].invalidated);
        self.domains[root].invalidated = cutoff;
        if cutoff != 0 {
            self.native_invalidations
                .insert((client, native_id.into()), cutoff);
        }
        Ok(root)
    }

    fn prune(&mut self, root: usize) -> Result<(), BrokerError> {
        let mut retained = Vec::new();
        for slot in &self.domains[root].slots {
            if !slot.finished()? {
                retained.push(Arc::clone(slot));
            }
        }
        self.domains[root].slots = retained;
        Ok(())
    }

    fn unresolved_client(&self, client: &str, except: Option<usize>) -> bool {
        self.entries.iter().any(|entry| {
            let root = self.root(entry.domain);
            entry.client == client && Some(root) != except && self.domains[root].unresolved
        })
    }

    fn retire_unresolved(&mut self, root: usize) -> Result<(), BrokerError> {
        if self.domains[root].unresolved {
            let stamp = self.tick()?;
            let clients: Vec<_> = self
                .entries
                .iter()
                .filter(|entry| self.root(entry.domain) == root)
                .map(|entry| entry.client.clone())
                .collect();
            for client in clients {
                self.client_invalidations.insert(client, stamp);
            }
            self.domains[root].unresolved = false;
        }
        Ok(())
    }

    fn reconcile(&mut self) -> Result<(), BrokerError> {
        for root in 0..self.domains.len() {
            if self.domains[root].parent != root {
                continue;
            }
            self.domains[root]
                .logout_slots
                .retain(|slot| !matches!(slot.finished(), Ok(true)));
            if self.domains[root].unresolved
                && self.domains[root].phase == Phase::Unconfirmed
                && self.domains[root].logout_slots.is_empty()
                && self.domains[root]
                    .slots
                    .iter()
                    .all(|slot| matches!(slot.finished(), Ok(true)))
            {
                // Only real confirmed teardown (or an abandoned, never-started
                // owner) retires the broad gate. Earlier admissions stay stale.
                self.retire_unresolved(root)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Acquisition {
    Login,
    Status,
    Connect,
    Refresh,
}

struct Ticket {
    owner: Arc<SessionCoordinator>,
    entry: usize,
    stamp: u64,
    admitted_binding: Option<String>,
    slot: Arc<Slot>,
    logout: bool,
    finished: bool,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.finished {
            self.slot.abandon();
            if self.logout
                && let Ok(mut state) = self.owner.state.lock()
            {
                let root = state.root(state.entries[self.entry].domain);
                state.domains[root].phase = Phase::Unconfirmed;
            }
        }
    }
}

struct SessionCoordinator {
    state: Mutex<Registry>,
    broker: Arc<dyn Broker>,
    drain_limit: Duration,
}

impl SessionCoordinator {
    fn new(broker: Arc<dyn Broker>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Registry::default()),
            broker,
            drain_limit: DRAIN_LIMIT,
        })
    }

    fn state(&self) -> Result<MutexGuard<'_, Registry>, BrokerError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| BrokerError::CoordinationFailed)?;
        if state.failed {
            Err(BrokerError::CoordinationFailed)
        } else {
            state.reconcile()?;
            Ok(state)
        }
    }

    fn begin(
        self: &Arc<Self>,
        options: &EntraOptions,
        kind: Acquisition,
        credential: Option<&WindowsCredential>,
    ) -> Result<Ticket, BrokerError> {
        let mut state = self.state()?;
        let entry = state.entry(options)?;
        let root = if let Some(credential) = credential {
            state.bind(entry, &credential.account_id)?
        } else {
            state.root(state.entries[entry].domain)
        };
        state.prune(root)?;
        let domain = &state.domains[root];
        if matches!(domain.phase, Phase::SigningOut | Phase::Unconfirmed)
            || (domain.phase == Phase::SignedOut && kind != Acquisition::Login)
            || credential.is_some_and(|credential| credential.generation <= domain.invalidated)
        {
            return Err(BrokerError::SessionInvalidated);
        }
        if !domain.slots.is_empty() {
            return Err(BrokerError::OperationInProgress);
        }
        let admitted_binding = state
            .entries
            .iter()
            .filter(|entry| state.root(entry.domain) == root)
            .find_map(|entry| entry.binding.clone());
        let stamp = state.tick()?;
        let slot = Arc::new(Slot::default());
        state.domains[root].slots.push(Arc::clone(&slot));
        Ok(Ticket {
            owner: Arc::clone(self),
            entry,
            stamp,
            admitted_binding,
            slot,
            logout: false,
            finished: false,
        })
    }

    fn publish(
        &self,
        ticket: &Ticket,
        native_id: &str,
        kind: Acquisition,
    ) -> Result<(), BrokerError> {
        let mut state = self.state()?;
        let root = state.bind(ticket.entry, native_id)?;
        let client = state.entries[ticket.entry].client.clone();
        let domain = &state.domains[root];
        if ticket.stamp <= domain.invalidated
            || matches!(domain.phase, Phase::SigningOut | Phase::Unconfirmed)
            || (domain.phase == Phase::SignedOut && kind != Acquisition::Login)
        {
            return Err(BrokerError::SessionInvalidated);
        }
        let except =
            (kind == Acquisition::Login && domain.phase == Phase::SignedOut).then_some(root);
        if state.unresolved_client(&client, except) {
            return Err(cleanup_pending());
        }
        if ticket.admitted_binding.is_none()
            && state
                .client_invalidations
                .get(&client)
                .is_some_and(|cutoff| ticket.stamp <= *cutoff)
        {
            return Err(BrokerError::SessionInvalidated);
        }
        state.domains[root].phase = Phase::Active;
        if except.is_some() {
            state.domains[root].unresolved = false;
        }
        // This locked check is publication, not the native completion callback.
        Ok(())
    }

    async fn acquire(
        self: &Arc<Self>,
        options: EntraOptions,
        kind: Acquisition,
        interaction: Interaction,
    ) -> Result<Arc<WindowsCredential>, BrokerError> {
        let mut ticket = self.begin(&options, kind, None)?;
        let binding = ticket.admitted_binding.clone();
        let interaction = if kind == Acquisition::Status {
            Interaction::Silent
        } else {
            interaction
        };
        let audiences = match kind {
            Acquisition::Login | Acquisition::Status => vec![
                TokenAudience::Fabric,
                TokenAudience::OneLake,
                TokenAudience::Arm,
            ],
            Acquisition::Connect | Acquisition::Refresh => vec![TokenAudience::Fabric],
        };
        let running = self
            .broker
            .acquire(options.clone(), binding, audiences, interaction);
        ticket.slot.attach(running.control)?;
        let timeout = match interaction {
            Interaction::Silent => SILENT_LIMIT,
            Interaction::Allowed => INTERACTIVE_LIMIT,
        };
        let session = tokio::time::timeout(timeout, running.result)
            .await
            .map_err(|_| BrokerError::TimedOut)??;
        let credential = Arc::new(WindowsCredential {
            options,
            account_id: session.account_id,
            metadata: session
                .tokens
                .into_iter()
                .map(|token| TokenMetadata {
                    scope: token.audience.scope(),
                    expires_on: token.expires_on,
                })
                .collect(),
            coordinator: Arc::clone(self),
            generation: ticket.stamp,
        });
        self.publish(&ticket, &credential.account_id, kind)?;
        ticket.finished = true;
        Ok(credential)
    }

    async fn refresh(
        self: &Arc<Self>,
        credential: &WindowsCredential,
        audience: TokenAudience,
    ) -> Result<AccessToken, BrokerError> {
        let mut ticket = self.begin(&credential.options, Acquisition::Refresh, Some(credential))?;
        let running = self.broker.acquire(
            credential.options.clone(),
            Some(credential.account_id.clone()),
            vec![audience],
            Interaction::Silent,
        );
        ticket.slot.attach(running.control)?;
        let session = tokio::time::timeout(SILENT_LIMIT, running.result)
            .await
            .map_err(|_| BrokerError::TimedOut)??;
        if session.account_id != credential.account_id || session.tokens.len() != 1 {
            return Err(BrokerError::IdentityMismatch);
        }
        let token = session
            .tokens
            .into_iter()
            .next()
            .ok_or(BrokerError::InvalidResponse)?;
        if token.audience != audience || token.secret.is_empty() {
            return Err(BrokerError::InvalidResponse);
        }
        let expires = OffsetDateTime::from_unix_timestamp(token.expires_on)
            .map_err(|_| BrokerError::InvalidResponse)?;
        let token = AccessToken::new(token.secret, expires);
        self.publish(&ticket, &credential.account_id, Acquisition::Refresh)?;
        ticket.finished = true;
        Ok(token)
    }

    fn begin_logout(self: &Arc<Self>, options: &EntraOptions) -> Result<Ticket, BrokerError> {
        let mut state = self.state()?;
        let entry = state.entry(options)?;
        let root = state.root(state.entries[entry].domain);
        if state.domains[root].phase == Phase::SigningOut {
            return Err(BrokerError::OperationInProgress);
        }
        let stamp = state.tick()?;
        state.domains[root].invalidated = stamp;
        state.domains[root].phase = Phase::SigningOut;
        let bindings: Vec<_> = state
            .entries
            .iter()
            .filter(|entry| state.root(entry.domain) == root)
            .filter_map(|entry| {
                entry
                    .binding
                    .as_ref()
                    .map(|id| (entry.client.clone(), id.clone()))
            })
            .collect();
        state.domains[root].unresolved = bindings.is_empty();
        if state.domains[root].unresolved {
            state
                .client_invalidations
                .insert(options.client().into(), stamp);
        }
        for binding in bindings {
            state.native_invalidations.insert(binding, stamp);
        }
        // Reserve before releasing the lock; a dropped future cannot roll back
        // invalidation or allow a later acquisition past this operation.
        let slot = Arc::new(Slot::default());
        state.domains[root].logout_slots.push(Arc::clone(&slot));
        Ok(Ticket {
            owner: Arc::clone(self),
            entry,
            stamp,
            admitted_binding: None,
            slot,
            logout: true,
            finished: false,
        })
    }

    async fn sign_out(self: &Arc<Self>, options: EntraOptions) -> Result<(), BrokerError> {
        let mut ticket = self.begin_logout(&options)?;
        let draining = {
            let state = self.state()?;
            let root = state.root(state.entries[ticket.entry].domain);
            state.domains[root].slots.clone()
        };
        for slot in &draining {
            slot.cancel();
        }
        let drained = tokio::time::timeout(self.drain_limit, async {
            for slot in &draining {
                slot.wait().await?;
            }
            Ok::<(), BrokerError>(())
        })
        .await
        .map_err(|_| cleanup_pending())?;
        drained?;
        let binding = {
            let mut state = self.state()?;
            let root = state.root(state.entries[ticket.entry].domain);
            state.prune(root)?;
            if !state.domains[root].slots.is_empty() {
                return Err(cleanup_pending());
            }
            state.domains[root].slots.push(Arc::clone(&ticket.slot));
            state
                .entries
                .iter()
                .filter(|entry| state.root(entry.domain) == root)
                .find_map(|entry| entry.binding.clone())
        };
        let running = self.broker.logout(options, binding);
        ticket.slot.attach(running.control)?;
        let native_id = tokio::time::timeout(SILENT_LIMIT, running.result)
            .await
            .map_err(|_| BrokerError::TimedOut)??;
        // A successful callback is not confirmation that the worker released its
        // apartment/window. A dropped logout remains unconfirmed during this wait.
        tokio::time::timeout(self.drain_limit, ticket.slot.wait())
            .await
            .map_err(|_| cleanup_pending())??;
        let joined = {
            let mut state = self.state()?;
            let root = if let Some(native_id) = &native_id {
                state.bind(ticket.entry, native_id)?
            } else {
                state.root(state.entries[ticket.entry].domain)
            };
            if state.domains[root].invalidated != ticket.stamp {
                return Err(BrokerError::SessionInvalidated);
            }
            state.domains[root].slots.clone()
        };
        for slot in &joined {
            slot.cancel();
        }
        tokio::time::timeout(self.drain_limit, async {
            for slot in &joined {
                slot.wait().await?;
            }
            Ok::<(), BrokerError>(())
        })
        .await
        .map_err(|_| cleanup_pending())??;
        {
            let mut state = self.state()?;
            let root = state.root(state.entries[ticket.entry].domain);
            state.prune(root)?;
            if state.domains[root].invalidated != ticket.stamp
                || !state.domains[root].slots.is_empty()
                || !state.domains[root].logout_slots.is_empty()
            {
                return Err(cleanup_pending());
            }
            state.retire_unresolved(root)?;
            state.domains[root].phase = Phase::SignedOut;
        }
        ticket.finished = true;
        Ok(())
    }
}

fn cleanup_pending() -> BrokerError {
    let (active, limit) = wam::OperationControl::worker_counts();
    BrokerError::CleanupPending { active, limit }
}

fn coordinator() -> &'static Arc<SessionCoordinator> {
    static COORDINATOR: OnceLock<Arc<SessionCoordinator>> = OnceLock::new();
    COORDINATOR.get_or_init(|| SessionCoordinator::new(Arc::new(NativeBroker)))
}

/// An account/tenant-bound WAM credential with process-local logout invalidation.
///
/// SDK refresh is silent. All existing login/connect/sign-out entry points share
/// one bounded process coordinator. Explicit logout permanently invalidates old
/// credentials, even when native logout fails or is cancelled after starting.
/// Already returned tokens and other processes cannot be revoked by this object.
pub struct WindowsCredential {
    options: EntraOptions,
    account_id: String,
    metadata: Vec<TokenMetadata>,
    coordinator: Arc<SessionCoordinator>,
    generation: u64,
}

impl WindowsCredential {
    /// Connects silently for Fabric only, using the explicit selected identity.
    ///
    /// # Errors
    ///
    /// Returns sanitized native, busy/capacity or invalidated-session failures.
    /// After logout, complete sign-out and invoke explicit `login` to reopen.
    pub async fn connect(options: EntraOptions) -> Result<Arc<Self>, BrokerError> {
        coordinator()
            .acquire(options, Acquisition::Connect, Interaction::Silent)
            .await
    }

    /// Silently checks Fabric, OneLake and ARM without reopening signed-out state.
    ///
    /// This can renew the explicitly selected active broker session, but never
    /// authorizes UI. Unlike explicit `login(options, false)`, it cannot reopen a
    /// confirmed sign-out or recover unconfirmed logout. No partial credential
    /// is published if any audience fails.
    ///
    /// # Errors
    ///
    /// Returns sanitized native, interaction, lifecycle or capacity failures.
    /// Signed-out/unconfirmed state is rejected before native broker work.
    pub async fn status(options: EntraOptions) -> Result<Arc<Self>, BrokerError> {
        coordinator()
            .acquire(options, Acquisition::Status, Interaction::Silent)
            .await
    }

    /// Acquires Fabric, OneLake and ARM in order, publishing no partial session.
    ///
    /// Only explicit caller permission permits interaction. Cancellation does not
    /// roll back WAM cache changes. Reopening confirmed sign-out issues a new
    /// generation; previously returned credentials remain invalid. Passing
    /// `false` disables UI, not the explicit login intent. Use `status` for a
    /// silent check that must not reopen signed-out state.
    ///
    /// # Errors
    ///
    /// Returns sanitized native, interaction, lifecycle or capacity failures.
    pub async fn login(
        options: EntraOptions,
        allow_interaction: bool,
    ) -> Result<Arc<Self>, BrokerError> {
        coordinator()
            .acquire(
                options,
                Acquisition::Login,
                if allow_interaction {
                    Interaction::Allowed
                } else {
                    Interaction::Silent
                },
            )
            .await
    }

    /// Historical metadata from acquisition, not current status or a new token.
    pub fn metadata(&self) -> &[TokenMetadata] {
        &self.metadata
    }
}

impl fmt::Debug for WindowsCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsCredential").finish_non_exhaustive()
    }
}

#[async_trait]
impl TokenCredential for WindowsCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let audience = match scopes {
            ["https://api.fabric.microsoft.com/.default"] => TokenAudience::Fabric,
            ["https://storage.azure.com/.default"] => TokenAudience::OneLake,
            ["https://management.azure.com/.default"] => TokenAudience::Arm,
            _ => {
                return Err(Error::with_message(
                    ErrorKind::Credential,
                    "unsupported credential scope",
                ));
            }
        };
        self.coordinator
            .refresh(self, audience)
            .await
            .map_err(|error| Error::new(ErrorKind::Credential, error))
    }
}

/// Invalidates this process's application-account session, then signs out WAM.
///
/// The first poll starts invalidation; dropping an unpolled future does nothing.
/// Cancellation/failure after starting never restores old credentials. Explicit
/// retry can finish sign-out. This does not remove a Windows account, revoke
/// already returned tokens or coordinate other processes using the client ID.
///
/// # Errors
///
/// Returns sanitized native or cleanup/capacity failures, never false success.
pub async fn sign_out(options: EntraOptions) -> Result<(), BrokerError> {
    coordinator().sign_out(options).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Waker},
    };
    use tokio::sync::{Notify, oneshot};

    const TENANT: &str = "11111111-2222-3333-4444-555555555555";
    const OTHER: &str = "22222222-2222-3333-4444-555555555555";
    const FABRIC_SCOPES: &[&str] = &["https://api.fabric.microsoft.com/.default"];
    const ARM_SCOPES: &[&str] = &["https://management.azure.com/.default"];

    #[derive(Default)]
    struct FakeControl {
        cancelled: AtomicBool,
        ended: AtomicBool,
        uncertain: AtomicBool,
        changed: Notify,
    }

    impl FakeControl {
        fn end(&self) {
            self.ended.store(true, Ordering::Release);
            self.changed.notify_waiters();
        }

        fn end_uncertain(&self) {
            self.uncertain.store(true, Ordering::Release);
            self.end();
        }
    }

    #[async_trait]
    impl Cleanup for FakeControl {
        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Release);
        }

        fn ended(&self) -> bool {
            self.ended.load(Ordering::Acquire)
        }

        fn confirmed(&self) -> bool {
            self.ended() && !self.uncertain.load(Ordering::Acquire)
        }

        async fn wait(&self) -> Result<(), BrokerError> {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.ended() {
                    return if self.confirmed() {
                        Ok(())
                    } else {
                        Err(cleanup_pending())
                    };
                }
                changed.await;
            }
        }
    }

    struct AcquireCall {
        binding: Option<String>,
        audiences: Vec<TokenAudience>,
        interaction: Interaction,
        control: Arc<FakeControl>,
        sender: oneshot::Sender<Result<Session, BrokerError>>,
    }

    impl AcquireCall {
        fn succeed(self, id: &str, ended: bool) {
            if ended {
                self.control.end();
            }
            let tokens = self
                .audiences
                .iter()
                .map(|audience| SessionToken {
                    audience: *audience,
                    expires_on: 2_000_000_000,
                    secret: "distinctive-offline-only-secret".into(),
                })
                .collect();
            self.sender
                .send(Ok(Session {
                    account_id: id.into(),
                    tokens,
                }))
                .unwrap_or_else(|_| panic!("fake acquisition receiver closed"));
        }

        fn fail(self, error: BrokerError) {
            self.control.end();
            self.sender
                .send(Err(error))
                .unwrap_or_else(|_| panic!("fake acquisition receiver closed"));
        }
    }

    struct LogoutCall {
        binding: Option<String>,
        control: Arc<FakeControl>,
        sender: oneshot::Sender<Result<Option<String>, BrokerError>>,
    }

    impl LogoutCall {
        fn complete(self, result: Result<Option<String>, BrokerError>, ended: bool) {
            if ended {
                self.control.end();
            }
            self.sender
                .send(result)
                .unwrap_or_else(|_| panic!("fake logout receiver closed"));
        }
    }

    #[derive(Default)]
    struct FakeBroker {
        acquisitions: Mutex<VecDeque<AcquireCall>>,
        logouts: Mutex<VecDeque<LogoutCall>>,
    }

    fn channel<T: Send + 'static>() -> (
        Arc<FakeControl>,
        oneshot::Sender<Result<T, BrokerError>>,
        Running<T>,
    ) {
        let control = Arc::new(FakeControl::default());
        let (sender, receiver) = oneshot::channel();
        let running = Running {
            control: control.clone(),
            result: Box::pin(async move {
                receiver
                    .await
                    .map_err(|_| BrokerError::CoordinationFailed)?
            }),
        };
        (control, sender, running)
    }

    impl Broker for FakeBroker {
        fn acquire(
            &self,
            _options: EntraOptions,
            binding: Option<String>,
            audiences: Vec<TokenAudience>,
            interaction: Interaction,
        ) -> Running<Session> {
            let (control, sender, running) = channel();
            self.acquisitions.lock().unwrap().push_back(AcquireCall {
                binding,
                audiences,
                interaction,
                control,
                sender,
            });
            running
        }

        fn logout(
            &self,
            _options: EntraOptions,
            binding: Option<String>,
        ) -> Running<Option<String>> {
            let (control, sender, running) = channel();
            self.logouts.lock().unwrap().push_back(LogoutCall {
                binding,
                control,
                sender,
            });
            running
        }
    }

    impl FakeBroker {
        fn acquisition(&self) -> AcquireCall {
            self.acquisitions.lock().unwrap().pop_front().unwrap()
        }

        fn logout(&self) -> LogoutCall {
            self.logouts.lock().unwrap().pop_front().unwrap()
        }

        fn idle(&self) {
            assert!(self.acquisitions.lock().unwrap().is_empty());
            assert!(self.logouts.lock().unwrap().is_empty());
        }
    }

    fn setup() -> (Arc<SessionCoordinator>, Arc<FakeBroker>) {
        let broker = Arc::new(FakeBroker::default());
        let owner = SessionCoordinator::new(broker.clone());
        (owner, broker)
    }

    fn options(tenant: &str, client: &str, account: &str) -> EntraOptions {
        EntraOptions::new(tenant, client, account).unwrap()
    }

    fn selected() -> EntraOptions {
        options(TENANT, TENANT, "user@example.test")
    }

    fn pending<F: Future>(future: Pin<&mut F>) {
        assert!(
            future
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    }

    async fn connect(
        owner: &Arc<SessionCoordinator>,
        broker: &FakeBroker,
        options: EntraOptions,
        id: &str,
    ) -> Arc<WindowsCredential> {
        let future = owner.acquire(options, Acquisition::Connect, Interaction::Silent);
        tokio::pin!(future);
        pending(future.as_mut());
        let call = broker.acquisition();
        assert_eq!(call.audiences, vec![TokenAudience::Fabric]);
        assert_eq!(call.interaction, Interaction::Silent);
        call.succeed(id, true);
        future.await.unwrap()
    }

    async fn logout(owner: &Arc<SessionCoordinator>, broker: &FakeBroker, options: EntraOptions) {
        let future = owner.sign_out(options);
        tokio::pin!(future);
        pending(future.as_mut());
        broker.logout().complete(Ok(Some("native-id".into())), true);
        future.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn login_connect_and_real_sdk_refresh_share_the_coordinator_and_stay_silent() {
        let (owner, broker) = setup();
        let future = owner.acquire(selected(), Acquisition::Login, Interaction::Allowed);
        tokio::pin!(future);
        pending(future.as_mut());
        let call = broker.acquisition();
        assert_eq!(
            call.audiences,
            vec![
                TokenAudience::Fabric,
                TokenAudience::OneLake,
                TokenAudience::Arm
            ]
        );
        assert_eq!(call.interaction, Interaction::Allowed);
        call.succeed("native-id", true);
        let credential = future.await.unwrap();
        assert_eq!(credential.metadata().len(), 3);
        assert_eq!(format!("{credential:?}"), "WindowsCredential { .. }");
        let token = credential.get_token(ARM_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        let call = broker.acquisition();
        assert_eq!(call.interaction, Interaction::Silent);
        assert_eq!(call.binding.as_deref(), Some("native-id"));
        assert_eq!(call.audiences, vec![TokenAudience::Arm]);
        call.succeed("native-id", true);
        assert!(token.await.is_ok());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_is_silent_and_requests_all_three_audiences_for_fresh_and_active_sessions() {
        let (owner, broker) = setup();
        for (binding, interaction) in [
            (None, Interaction::Allowed),
            (Some("native-id"), Interaction::Silent),
        ] {
            let status = owner.acquire(selected(), Acquisition::Status, interaction);
            tokio::pin!(status);
            pending(status.as_mut());
            let call = broker.acquisition();
            assert_eq!(call.binding.as_deref(), binding);
            assert_eq!(call.interaction, Interaction::Silent);
            assert_eq!(
                call.audiences,
                vec![
                    TokenAudience::Fabric,
                    TokenAudience::OneLake,
                    TokenAudience::Arm
                ]
            );
            call.succeed("native-id", true);
            let credential = status.await.unwrap();
            assert_eq!(
                credential
                    .metadata()
                    .iter()
                    .map(|metadata| metadata.scope)
                    .collect::<Vec<_>>(),
                vec![
                    TokenAudience::Fabric.scope(),
                    TokenAudience::OneLake.scope(),
                    TokenAudience::Arm.scope(),
                ]
            );
        }
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_after_confirmed_logout_fails_before_broker_work() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        logout(&owner, &broker, selected()).await;
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Status, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_after_unconfirmed_logout_fails_before_broker_work() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let error = BrokerError::Provider {
            status: 5,
            code: 123,
        };
        broker.logout().complete(Err(error), true);
        assert_eq!(out.await, Err(error));
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Status, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_silent_login_reopens_confirmed_logout_but_not_old_credentials() {
        let (owner, broker) = setup();
        let old = connect(&owner, &broker, selected(), "native-id").await;
        logout(&owner, &broker, selected()).await;
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Status, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        let login = owner.acquire(selected(), Acquisition::Login, Interaction::Silent);
        tokio::pin!(login);
        pending(login.as_mut());
        let call = broker.acquisition();
        assert_eq!(call.binding.as_deref(), Some("native-id"));
        assert_eq!(call.interaction, Interaction::Silent);
        assert_eq!(
            call.audiences,
            vec![
                TokenAudience::Fabric,
                TokenAudience::OneLake,
                TokenAudience::Arm
            ]
        );
        call.succeed("native-id", true);
        let reopened = login.await.unwrap();
        assert!(old.get_token(FABRIC_SCOPES, None).await.is_err());
        let token = reopened.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("native-id", true);
        assert!(token.await.is_ok());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_rejects_unknown_or_combined_scopes_before_native_access() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        for scopes in [
            vec![],
            vec!["https://evil.example/.default"],
            vec![
                TokenAudience::Fabric.scope(),
                TokenAudience::OneLake.scope(),
            ],
        ] {
            let error = credential.get_token(&scopes, None).await.unwrap_err();
            assert_eq!(error.to_string(), "unsupported credential scope");
        }
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_initial_tenant_acquisitions_join_before_native_id_arrives_and_logout_wins() {
        let (owner, broker) = setup();
        let first = owner.acquire(selected(), Acquisition::Connect, Interaction::Silent);
        tokio::pin!(first);
        pending(first.as_mut());
        let initial = broker.acquisition();
        let native = Arc::clone(&initial.control);
        let second = owner
            .acquire(
                options(OTHER, TENANT, "USER@example.test"),
                Acquisition::Connect,
                Interaction::Silent,
            )
            .await;
        assert_eq!(second.unwrap_err(), BrokerError::OperationInProgress);
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        assert!(native.cancelled.load(Ordering::Acquire));
        assert!(broker.logouts.lock().unwrap().is_empty());
        initial.succeed("native-id", false);
        assert_eq!(first.await.unwrap_err(), BrokerError::SessionInvalidated);
        native.end();
        pending(out.as_mut());
        let call = broker.logout();
        assert_eq!(call.binding.as_deref(), Some("native-id"));
        call.complete(Ok(Some("native-id".into())), true);
        out.await.unwrap();
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_initial_alias_acquisitions_with_late_native_id_cannot_cross_pending_logout() {
        let (owner, broker) = setup();
        let first = owner.acquire(selected(), Acquisition::Connect, Interaction::Silent);
        let alias_options = options(OTHER, TENANT, "alias@example.test");
        let alias = owner.acquire(alias_options, Acquisition::Connect, Interaction::Silent);
        tokio::pin!(first, alias);
        pending(first.as_mut());
        let first_call = broker.acquisition();
        let first_control = Arc::clone(&first_call.control);
        pending(alias.as_mut());
        let alias_call = broker.acquisition();
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        alias_call.succeed("native-id", true);
        assert!(matches!(
            alias.await.unwrap_err(),
            BrokerError::CleanupPending { .. }
        ));
        first_call.succeed("native-id", false);
        assert_eq!(first.await.unwrap_err(), BrokerError::SessionInvalidated);
        first_control.end();
        pending(out.as_mut());
        broker.logout().complete(Ok(Some("native-id".into())), true);
        out.await.unwrap();
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn alias_callback_buffered_before_poll_observes_native_id_logout_generation() {
        let (owner, broker) = setup();
        let _credential = connect(&owner, &broker, selected(), "native-id").await;
        let alias = owner.acquire(
            options(OTHER, TENANT, "alias@example.test"),
            Acquisition::Connect,
            Interaction::Silent,
        );
        tokio::pin!(alias);
        pending(alias.as_mut());
        let call = broker.acquisition();
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let logout = broker.logout();
        call.succeed("native-id", true);
        assert_eq!(alias.await.unwrap_err(), BrokerError::SessionInvalidated);
        logout.complete(Ok(Some("native-id".into())), true);
        out.await.unwrap();
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn late_alias_merge_keeps_the_other_domains_live_permit_and_invalidates_both() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        let alias_options = options(OTHER, TENANT, "alias@example.test");
        let alias = owner.acquire(
            alias_options.clone(),
            Acquisition::Connect,
            Interaction::Silent,
        );
        tokio::pin!(alias);
        pending(alias.as_mut());
        let alias_call = broker.acquisition();
        let token = credential.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        let refresh = broker.acquisition();
        alias_call.succeed("native-id", true);
        let alias = alias.await.unwrap();
        assert_eq!(
            owner
                .acquire(
                    alias_options.clone(),
                    Acquisition::Connect,
                    Interaction::Silent
                )
                .await
                .unwrap_err(),
            BrokerError::OperationInProgress
        );
        let out = owner.sign_out(alias_options);
        tokio::pin!(out);
        pending(out.as_mut());
        assert!(refresh.control.cancelled.load(Ordering::Acquire));
        assert!(broker.logouts.lock().unwrap().is_empty());
        refresh.succeed("native-id", true);
        assert!(token.await.is_err());
        pending(out.as_mut());
        broker.logout().complete(Ok(Some("native-id".into())), true);
        out.await.unwrap();
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        assert!(alias.get_token(FABRIC_SCOPES, None).await.is_err());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sibling_tenant_credentials_are_invalidated_without_affecting_other_clients() {
        let (owner, broker) = setup();
        let _first = connect(&owner, &broker, selected(), "native-id").await;
        let sibling = connect(
            &owner,
            &broker,
            options(OTHER, TENANT, "USER@example.test"),
            "native-id",
        )
        .await;
        logout(&owner, &broker, selected()).await;
        assert!(sibling.get_token(FABRIC_SCOPES, None).await.is_err());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refresh_callback_cannot_publish_after_logout_and_old_credentials_never_reopen() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        let token = credential.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        let refresh = broker.acquisition();
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        refresh.succeed("native-id", true);
        assert!(token.await.is_err());
        pending(out.as_mut());
        broker.logout().complete(Ok(Some("native-id".into())), true);
        out.await.unwrap();
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Connect, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        let login = owner.acquire(selected(), Acquisition::Login, Interaction::Silent);
        tokio::pin!(login);
        pending(login.as_mut());
        let call = broker.acquisition();
        assert_eq!(call.binding.as_deref(), Some("native-id"));
        call.succeed("native-id", true);
        let fresh = login.await.unwrap();
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        let token = fresh.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("native-id", true);
        assert!(token.await.is_ok());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logout_failure_keeps_invalidation_and_requires_explicit_same_identity_retry() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let error = BrokerError::Provider {
            status: 5,
            code: 123,
        };
        broker.logout().complete(Err(error), true);
        assert_eq!(out.await, Err(error));
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Login, Interaction::Allowed)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        logout(&owner, &broker, selected()).await;
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_started_logout_does_not_restore_state_or_release_native_work() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        let mut out = Box::pin(owner.sign_out(selected()));
        pending(out.as_mut());
        let call = broker.logout();
        drop(out);
        assert!(call.control.cancelled.load(Ordering::Acquire));
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        let retry = owner.sign_out(selected());
        tokio::pin!(retry);
        pending(retry.as_mut());
        assert!(broker.logouts.lock().unwrap().is_empty());
        call.control.end();
        pending(retry.as_mut());
        broker.logout().complete(Ok(Some("native-id".into())), true);
        retry.await.unwrap();
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_logout_callback_waits_for_actual_teardown_before_reporting_success() {
        let (owner, broker) = setup();
        let _credential = connect(&owner, &broker, selected(), "native-id").await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let call = broker.logout();
        let control = Arc::clone(&call.control);
        call.complete(Ok(Some("native-id".into())), false);
        pending(out.as_mut());
        assert!(!control.ended());
        control.end();
        out.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unconfirmed_native_cleanup_cannot_report_success_or_admit_a_replacement() {
        let (owner, broker) = setup();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let call = broker.logout();
        call.control.end_uncertain();
        call.complete(Ok(Some("native-id".into())), false);
        assert!(matches!(out.await, Err(BrokerError::CleanupPending { .. })));
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        assert!(matches!(
            owner.sign_out(selected()).await,
            Err(BrokerError::CleanupPending { .. })
        ));
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn constructing_and_dropping_unpolled_logout_is_intentionally_a_noop() {
        let (owner, broker) = setup();
        drop(owner.sign_out(selected()));
        drop(sign_out(selected()));
        assert!(owner.state().unwrap().entries.is_empty());
        broker.idle();
        let credential = connect(&owner, &broker, selected(), "native-id").await;
        drop(owner.sign_out(selected()));
        let token = credential.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("native-id", true);
        assert!(token.await.is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unrelated_principal_and_client_remain_usable_during_bound_logout() {
        let (owner, broker) = setup();
        let _selected = connect(&owner, &broker, selected(), "native-id").await;
        let unrelated = connect(
            &owner,
            &broker,
            options(TENANT, TENANT, "other@example.test"),
            "other-native-id",
        )
        .await;
        let other_client = connect(
            &owner,
            &broker,
            options(TENANT, OTHER, "user@example.test"),
            "native-id",
        )
        .await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let call = broker.logout();
        for (credential, id) in [
            (&unrelated, "other-native-id"),
            (&other_client, "native-id"),
        ] {
            let token = credential.get_token(FABRIC_SCOPES, None);
            tokio::pin!(token);
            pending(token.as_mut());
            broker.acquisition().succeed(id, true);
            assert!(token.await.is_ok());
        }
        call.complete(Ok(Some("native-id".into())), true);
        out.await.unwrap();
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_logout_barrier_is_temporary_for_an_unrelated_validated_principal() {
        let (owner, broker) = setup();
        let unrelated = connect(
            &owner,
            &broker,
            options(TENANT, TENANT, "other@example.test"),
            "other-native-id",
        )
        .await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let logout = broker.logout();
        assert!(logout.binding.is_none());
        let token = unrelated.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("other-native-id", true);
        assert!(token.await.is_err());
        logout.complete(Ok(Some("native-id".into())), true);
        out.await.unwrap();
        let token = unrelated.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("other-native-id", true);
        assert!(token.await.is_ok());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_absence_allows_same_client_connect_login_and_existing_credential_refresh() {
        let (owner, broker) = setup();
        let unrelated = connect(
            &owner,
            &broker,
            options(TENANT, TENANT, "existing@example.test"),
            "existing-id",
        )
        .await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let call = broker.logout();
        assert!(call.binding.is_none());
        call.complete(Ok(None), true);
        out.await.unwrap();

        let _connected = connect(
            &owner,
            &broker,
            options(OTHER, TENANT, "new@example.test"),
            "new-id",
        )
        .await;
        let login = owner.acquire(
            options(OTHER, TENANT, "login@example.test"),
            Acquisition::Login,
            Interaction::Silent,
        );
        tokio::pin!(login);
        pending(login.as_mut());
        broker.acquisition().succeed("login-id", true);
        assert!(login.await.is_ok());
        let token = unrelated.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("existing-id", true);
        assert!(token.await.is_ok());
        let _other_client = connect(
            &owner,
            &broker,
            options(TENANT, OTHER, "user@example.test"),
            "other-client-id",
        )
        .await;

        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Connect, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        let state = owner.state().unwrap();
        let selected = state
            .entries
            .iter()
            .find(|entry| entry.account == "user@example.test" && entry.client == TENANT)
            .unwrap();
        assert!(selected.binding.is_none());
        assert!(state.domains[state.root(selected.domain)].phase == Phase::SignedOut);
        drop(state);
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bound_distinct_refresh_overlapping_unknown_absence_succeeds_after_teardown() {
        let (owner, broker) = setup();
        let credential = connect(
            &owner,
            &broker,
            options(OTHER, TENANT, "distinct@example.test"),
            "distinct-native-id",
        )
        .await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let absence = broker.logout();
        assert!(absence.binding.is_none());

        let token = credential.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        let refresh = broker.acquisition();
        assert_eq!(refresh.binding.as_deref(), Some("distinct-native-id"));

        absence.complete(Ok(None), true);
        out.await.unwrap();
        refresh.succeed("distinct-native-id", true);
        assert!(token.await.is_ok());
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Connect, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bound_matching_id_refresh_overlapping_unknown_logout_is_still_invalidated() {
        let (owner, broker) = setup();
        let credential = connect(
            &owner,
            &broker,
            options(OTHER, TENANT, "alias@example.test"),
            "shared-native-id",
        )
        .await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let found = broker.logout();
        assert!(found.binding.is_none());

        let token = credential.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        let refresh = broker.acquisition();
        assert_eq!(refresh.binding.as_deref(), Some("shared-native-id"));

        found.complete(Ok(Some("shared-native-id".into())), true);
        pending(out.as_mut());
        assert!(refresh.control.cancelled.load(Ordering::Acquire));
        refresh.succeed("shared-native-id", true);
        assert!(token.await.is_err());
        out.await.unwrap();
        assert!(credential.get_token(FABRIC_SCOPES, None).await.is_err());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_absence_never_rehabilitates_the_selected_old_credential() {
        let (owner, broker) = setup();
        let selected_credential = connect(&owner, &broker, selected(), "native-id").await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        broker.logout().complete(Ok(None), true);
        out.await.unwrap();
        assert!(
            selected_credential
                .get_token(FABRIC_SCOPES, None)
                .await
                .is_err()
        );
        let _unrelated = connect(
            &owner,
            &broker,
            options(TENANT, TENANT, "other@example.test"),
            "other-id",
        )
        .await;
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn absence_callback_does_not_release_the_client_gate_before_actual_teardown() {
        let (owner, broker) = setup();
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let call = broker.logout();
        let control = Arc::clone(&call.control);
        call.complete(Ok(None), false);
        pending(out.as_mut());
        let unrelated = owner.acquire(
            options(TENANT, TENANT, "other@example.test"),
            Acquisition::Connect,
            Interaction::Silent,
        );
        tokio::pin!(unrelated);
        pending(unrelated.as_mut());
        broker.acquisition().succeed("other-id", true);
        assert!(matches!(
            unrelated.await.unwrap_err(),
            BrokerError::CleanupPending { .. }
        ));
        control.end();
        out.await.unwrap();
        let _unrelated = connect(
            &owner,
            &broker,
            options(TENANT, TENANT, "other@example.test"),
            "other-id",
        )
        .await;
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn absence_epoch_rejects_alias_results_started_before_or_during_unknown_logout() {
        let (owner, broker) = setup();
        let before = owner.acquire(
            options(OTHER, TENANT, "before@example.test"),
            Acquisition::Connect,
            Interaction::Silent,
        );
        tokio::pin!(before);
        pending(before.as_mut());
        let before_call = broker.acquisition();
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let out_call = broker.logout();
        let during = owner.acquire(
            options(OTHER, TENANT, "during@example.test"),
            Acquisition::Connect,
            Interaction::Silent,
        );
        tokio::pin!(during);
        pending(during.as_mut());
        let during_call = broker.acquisition();
        out_call.complete(Ok(None), true);
        out.await.unwrap();
        before_call.succeed("late-shared-id", true);
        during_call.succeed("late-shared-id", true);
        assert_eq!(before.await.unwrap_err(), BrokerError::SessionInvalidated);
        assert_eq!(during.await.unwrap_err(), BrokerError::SessionInvalidated);
        let _new = connect(
            &owner,
            &broker,
            options(OTHER, TENANT, "fresh@example.test"),
            "fresh-id",
        )
        .await;
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_unknown_logout_releases_only_after_actual_confirmed_teardown() {
        let (owner, broker) = setup();
        let unrelated = connect(
            &owner,
            &broker,
            options(TENANT, TENANT, "other@example.test"),
            "other-id",
        )
        .await;
        let out = owner.sign_out(selected());
        tokio::pin!(out);
        pending(out.as_mut());
        let call = broker.logout();
        let control = Arc::clone(&call.control);
        call.complete(Err(BrokerError::TimedOut), false);
        assert_eq!(out.await, Err(BrokerError::TimedOut));
        let token = unrelated.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("other-id", true);
        assert!(token.await.is_err());
        control.end();
        let token = unrelated.get_token(FABRIC_SCOPES, None);
        tokio::pin!(token);
        pending(token.as_mut());
        broker.acquisition().succeed("other-id", true);
        assert!(token.await.is_ok());
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Login, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::SessionInvalidated
        );
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_unknown_logout_does_not_block_unrelated_accounts_after_late_teardown() {
        let (owner, broker) = setup();
        let mut out = Box::pin(owner.sign_out(selected()));
        pending(out.as_mut());
        let call = broker.logout();
        drop(out);
        assert!(call.control.cancelled.load(Ordering::Acquire));
        assert!(!call.control.ended());
        assert!(owner.state().unwrap().unresolved_client(TENANT, None));
        call.control.end();
        let _unrelated = connect(
            &owner,
            &broker,
            options(TENANT, TENANT, "other@example.test"),
            "other-id",
        )
        .await;
        assert!(!owner.state().unwrap().unresolved_client(TENANT, None));
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_acquisition_retains_its_domain_until_late_native_teardown() {
        let (owner, broker) = setup();
        let mut future =
            Box::pin(owner.acquire(selected(), Acquisition::Connect, Interaction::Silent));
        pending(future.as_mut());
        let call = broker.acquisition();
        drop(future);
        assert!(call.control.cancelled.load(Ordering::Acquire));
        assert_eq!(
            owner
                .acquire(selected(), Acquisition::Connect, Interaction::Silent)
                .await
                .unwrap_err(),
            BrokerError::OperationInProgress
        );
        call.control.end();
        let _credential = connect(&owner, &broker, selected(), "native-id").await;
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn registry_retains_all_64_keys_and_exhaustion_is_actionable_not_no_account() {
        let (owner, broker) = setup();
        for index in 0..IDENTITY_LIMIT {
            let account = format!("user{index}@example.test");
            let id = format!("native-id-{index}");
            let _credential =
                connect(&owner, &broker, options(TENANT, TENANT, &account), &id).await;
        }
        let out = owner.sign_out(options(TENANT, TENANT, "user0@example.test"));
        tokio::pin!(out);
        pending(out.as_mut());
        broker
            .logout()
            .complete(Ok(Some("native-id-0".into())), true);
        out.await.unwrap();
        let error = owner
            .acquire(selected(), Acquisition::Connect, Interaction::Silent)
            .await
            .unwrap_err();
        assert_eq!(error, BrokerError::IdentityCapacityExhausted { limit: 64 });
        assert!(error.to_string().contains("restart this process"));
        assert_eq!(owner.state().unwrap().entries.len(), 64);
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_id_substitution_never_rebinds_an_existing_session() {
        let (owner, broker) = setup();
        let _credential = connect(&owner, &broker, selected(), "native-id").await;
        let next = owner.acquire(selected(), Acquisition::Connect, Interaction::Silent);
        tokio::pin!(next);
        pending(next.as_mut());
        let call = broker.acquisition();
        assert_eq!(call.binding.as_deref(), Some("native-id"));
        call.succeed("different-native-id", true);
        assert_eq!(next.await.unwrap_err(), BrokerError::IdentityMismatch);
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interaction_required_and_partial_login_failure_do_not_prompt_or_publish() {
        let (owner, broker) = setup();
        let future = owner.acquire(selected(), Acquisition::Login, Interaction::Silent);
        tokio::pin!(future);
        pending(future.as_mut());
        let call = broker.acquisition();
        assert_eq!(call.interaction, Interaction::Silent);
        call.fail(BrokerError::InteractionRequired);
        assert_eq!(future.await.unwrap_err(), BrokerError::InteractionRequired);
        assert!(owner.state().unwrap().entries[0].binding.is_none());
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_capacity_error_is_not_relabelled_as_an_absent_account() {
        let (owner, broker) = setup();
        let future = owner.acquire(selected(), Acquisition::Connect, Interaction::Silent);
        tokio::pin!(future);
        pending(future.as_mut());
        let error = BrokerError::CapacityExhausted {
            active: 4,
            limit: 4,
        };
        broker.acquisition().fail(error);
        assert_eq!(future.await.unwrap_err(), error);
        broker.idle();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn zero_drain_budget_fails_closed_without_launching_logout_over_pending_native_work() {
        let broker = Arc::new(FakeBroker::default());
        let owner = Arc::new(SessionCoordinator {
            state: Mutex::new(Registry::default()),
            broker: broker.clone(),
            drain_limit: Duration::ZERO,
        });
        let mut acquire =
            Box::pin(owner.acquire(selected(), Acquisition::Connect, Interaction::Silent));
        pending(acquire.as_mut());
        let call = broker.acquisition();
        assert!(matches!(
            owner.sign_out(selected()).await,
            Err(BrokerError::CleanupPending { .. })
        ));
        assert!(call.control.cancelled.load(Ordering::Acquire));
        broker.idle();
        drop(acquire);
        call.control.end();
    }

    #[test]
    fn monotonic_generation_overflow_and_poisoned_registry_fail_closed() {
        let (owner, _broker) = setup();
        owner.state().unwrap().clock = u64::MAX;
        assert!(matches!(
            owner.begin(&selected(), Acquisition::Connect, None),
            Err(BrokerError::CoordinationFailed)
        ));
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.state.lock().unwrap();
            panic!("synthetic coordinator poison");
        }));
        assert!(poisoned.is_err());
        assert!(matches!(
            owner.state(),
            Err(BrokerError::CoordinationFailed)
        ));
    }

    #[test]
    fn lifecycle_diagnostics_contain_no_identity_or_token_sentinels() {
        for error in [
            BrokerError::SessionInvalidated,
            BrokerError::CoordinationFailed,
            BrokerError::TimedOut,
            BrokerError::CapacityExhausted {
                active: 4,
                limit: 4,
            },
            BrokerError::CleanupPending {
                active: 4,
                limit: 4,
            },
        ] {
            let text = format!("{error:?}: {error}");
            for sentinel in [
                "distinctive-offline-only-secret",
                "user@example.test",
                "native-id",
            ] {
                assert!(!text.contains(sentinel));
            }
        }
    }
}
