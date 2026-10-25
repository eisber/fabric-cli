// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional Windows desktop WAM interop, isolated from safe-only consumers.
//!
//! The raw native API boundary is complemented by an experimental Entra adapter.
//! Callers explicitly select account/tenant/client and authorize interactive UX.
//! WAM owns token persistence; no local credential file or guessed expiry exists.
//! No account discovery, UI or token acquisition happens merely by linking it.
//!
//! Existing workspace crates retain `unsafe_code = "forbid"`. This crate alone
//! permits reviewed unsafe blocks inside its private native module. Disabling
//! `fabric-identity`'s `windows-broker` feature excludes this dependency entirely;
//! enabling it does not make the resulting binary transitively unsafe-free.

use thiserror::Error;

mod entra_contract;
pub use entra_contract::{EntraOptions, Interaction, TokenAudience};

#[cfg(windows)]
mod entra;
#[cfg(windows)]
pub use entra::{EntraSession, EntraToken, acquire, logout};

#[cfg(any(windows, test))]
mod request;

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

/// Sanitized native and Entra adapter errors, without provider text or secrets.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BrokerError {
    /// Required tenant, client or account input is missing or malformed.
    #[error("an explicit account, tenant GUID and client GUID are required")]
    InvalidConfiguration,
    /// WAM is unavailable; no alternate backend is attempted.
    #[error("the Windows Entra account provider is unavailable")]
    ProviderUnavailable,
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
