// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional Windows identity interop, isolated from safe-only consumers.
//!
//! The raw native API boundary is complemented by an experimental Entra adapter.
//! Callers explicitly select account/tenant/client, or authorize the native
//! account picker during login before acquiring a fully bound session.
//! WAM owns token persistence; no local credential file or guessed expiry exists.
//! No account discovery, UI or token acquisition happens merely by linking it.
//!
//! Existing workspace crates retain `unsafe_code = "forbid"`. This crate alone
//! permits reviewed unsafe blocks inside its private native module. Default
//! identity consumers exclude this crate; opting into Windows broker or
//! certificate-store support does not make a binary transitively unsafe-free.

use thiserror::Error;

#[cfg(all(windows, feature = "certificate-store"))]
mod certificate;
#[cfg(all(windows, feature = "certificate-store"))]
pub use certificate::{
    AllowedCertificateProvider, CertificateError, CertificateSelector, CertificateSignOperation,
    CertificateStage, CertificateStore, assertion_nonce, sign_ps256,
};

mod entra_contract;
pub use entra_contract::{EntraOptions, Interaction, TokenAudience};

#[cfg(windows)]
mod entra;
#[cfg(windows)]
pub use entra::{EntraSession, EntraToken, acquire, logout, logout_bound};

#[cfg(any(windows, test))]
mod selection;
#[cfg(windows)]
pub use selection::{
    AccountSelectionStage, select_account, select_account_with_progress, validate_selection,
};

#[cfg(any(windows, test))]
mod request;

#[cfg(any(windows, test))]
pub use request::OperationControl;

#[cfg(windows)]
#[allow(unsafe_code)]
mod native;

/// Safe, typed Windows request inputs; constructing one does not acquire a token.
#[cfg(windows)]
pub use windows::Security::{Authentication::Web::Core::WebTokenRequest, Credentials::WebAccount};

#[cfg(windows)]
pub use native::{BrokerRequest, BrokerResponse, NativeOperation, request_interactive};

/// Native stages reported without provider text or credential data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeStage {
    /// Initialize the dedicated Windows UI apartment.
    Apartment,
    /// Create the owned parent window.
    ParentWindow,
    /// Acquire the desktop broker activation factory.
    Factory,
    /// Start a request through the documented desktop interop interface.
    Request,
    /// Discover the explicitly configured Entra account provider.
    Provider,
    /// Enumerate or inspect accounts for an explicit selector.
    Account,
    /// Read and validate the native provider response.
    Response,
    /// Sign out this application's selected account.
    Logout,
    /// Register or observe asynchronous completion.
    Completion,
    /// Request native cancellation.
    Cancellation,
}

/// A response field name suitable for diagnostics, never its provider value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseField {
    /// The response must contain exactly one account/token record.
    RecordCount,
    /// The returned Windows account object is missing.
    Account,
    /// The returned stable native account identifier is empty or unavailable.
    AccountId,
    /// The returned username is missing or is not a valid work-account UPN.
    AccountName,
    /// The response property collection cannot be inspected.
    Properties,
    /// The authority is missing or is not an allowed public-cloud authority.
    Authority,
    /// A tenant-neutral response lacks a valid broker account tenant identifier.
    TenantId,
    /// The broker-issued expiry is missing, invalid or no longer in the future.
    Expiry,
    /// The requested access token is missing or empty.
    Token,
}

impl std::fmt::Display for ResponseField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RecordCount => "account/token record count",
            Self::Account => "Windows account",
            Self::AccountId => "Windows account ID",
            Self::AccountName => "work-account username",
            Self::Properties => "response properties",
            Self::Authority => "public-cloud Authority",
            Self::TenantId => "broker account TenantId",
            Self::Expiry => "TokenExpiresOn",
            Self::Token => "access token",
        })
    }
}

/// Sanitized native and Entra adapter errors, without provider text or secrets.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BrokerError {
    /// The bounded process registry cannot retain another identity safely.
    #[error(
        "the identity registry is full (limit {limit}); restart this process to select additional identities"
    )]
    IdentityCapacityExhausted {
        /// Maximum retained identity keys, including signed-out tombstones.
        limit: usize,
    },
    /// Native resources remain owned by existing workers.
    #[error(
        "broker worker capacity exhausted ({active}/{limit}); wait for cleanup or restart this process if cleanup remains pending; do not retry in a loop"
    )]
    CapacityExhausted {
        /// Reserved workers, including workers awaiting cleanup.
        active: usize,
        /// Fixed worker limit.
        limit: usize,
    },
    /// A matching identity/application operation is already active.
    #[error("an operation for this application account is already in progress")]
    OperationInProgress,
    /// Logout invalidated this credential or operation.
    #[error(
        "the application session was invalidated; complete sign-out and explicitly log in again"
    )]
    SessionInvalidated,
    /// Local waiting ended without confirmation of native cleanup.
    #[error(
        "broker cleanup remains pending ({active}/{limit} workers); wait for cleanup or restart this process; do not retry in a loop"
    )]
    CleanupPending {
        /// Reserved workers, including workers awaiting cleanup.
        active: usize,
        /// Fixed worker limit.
        limit: usize,
    },
    /// The native operation's absolute deadline elapsed.
    #[error("the broker operation timed out; native cleanup may remain pending")]
    TimedOut,
    /// Poisoned or exhausted lifecycle bookkeeping fails closed.
    #[error("broker lifecycle coordination failed; restart this process")]
    CoordinationFailed,
    /// Required tenant, client or account input is missing or malformed.
    #[error("an explicit account, tenant GUID and client GUID are required")]
    InvalidConfiguration,
    /// WAM is unavailable; no alternate backend is attempted.
    #[error("the Windows Entra account provider is unavailable")]
    ProviderUnavailable,
    /// A visible parent window could not be established for interactive sign-in.
    #[error(
        "interactive sign-in needs a visible desktop window; run the command in an interactive Windows terminal"
    )]
    InteractiveWindowUnavailable,
    /// The selected account has not been added or authorized for silent use.
    #[error("the selected identity requires interactive sign-in")]
    InteractionRequired,
    /// More than one native account matches the selector.
    #[error("the account selector matches multiple Windows accounts")]
    AmbiguousAccount,
    /// Account enumeration is unsupported or disallowed by the provider.
    #[error("the broker cannot enumerate the selected client's accounts (status {status})")]
    AccountsUnavailable {
        /// Numeric FindAllWebAccountsStatus, not WebTokenRequestStatus.
        status: i32,
    },
    /// The result belongs to a different account or tenant.
    #[error("the broker returned a different account or tenant")]
    IdentityMismatch,
    /// Required response fields are missing or invalid.
    #[error("the broker response lacks valid identity, token or expiry metadata")]
    InvalidResponse,
    /// One required field could not be validated; its value is never included.
    #[error("the broker response has missing or invalid {field} metadata")]
    ResponseMetadata {
        /// A fixed field label, not a provider string or credential.
        field: ResponseField,
    },
    /// A broker response status or numeric provider error was returned.
    #[error("the broker rejected the request (status {status}, code {code:#010x})")]
    Provider {
        /// Numeric WebTokenRequestStatus.
        status: i32,
        /// Numeric provider error; never provider response text.
        code: u32,
    },
    /// The request was explicitly cancelled or dropped before delivery.
    #[error("the broker request was cancelled")]
    Cancelled,
    /// The dedicated native UI thread could not be started.
    #[error("the broker UI thread could not be started")]
    ThreadStart,
    /// Completion notification failed; the worker still owns pending native cleanup.
    #[error(
        "Windows broker completion is unconfirmed (HRESULT {code:#010x}); native cleanup remains pending"
    )]
    CompletionUnconfirmed {
        /// Numeric HRESULT returned while registering completion.
        code: i32,
    },
    /// A native call failed; its numeric HRESULT is retained, not its message.
    #[error("Windows broker call failed at {stage:?} (HRESULT {code:#010x})")]
    Native {
        /// The failing native stage.
        stage: NativeStage,
        /// Numeric HRESULT returned by Windows.
        code: i32,
    },
}
