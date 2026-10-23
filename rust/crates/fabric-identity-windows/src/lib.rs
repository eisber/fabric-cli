// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional Windows desktop WAM interop, isolated from safe-only consumers.
//!
//! This is a native API boundary, not a Fabric authentication backend. Callers
//! configure the provider/request and explicitly authorize interactive UX. The
//! boundary does not discover accounts, select tenants, persist tokens, or
//! manufacture token expiry. No API is invoked merely by linking this crate.
//!
//! Existing workspace crates retain `unsafe_code = "forbid"`. This crate alone
//! permits reviewed unsafe blocks inside its private native module. Disabling
//! `fabric-identity`'s `windows-broker` feature excludes this dependency entirely;
//! enabling it does not make the resulting binary transitively unsafe-free.

use thiserror::Error;

#[cfg(any(windows, test))]
mod request;

#[cfg(windows)]
#[allow(unsafe_code)]
mod native;

/// Safe, typed Windows request inputs; constructing one does not acquire a token.
#[cfg(windows)]
pub use windows::Security::{Authentication::Web::Core::WebTokenRequest, Credentials::WebAccount};

#[cfg(windows)]
pub use native::{BrokerRequest, BrokerResponse, request_interactive};

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
    /// Register or observe asynchronous completion.
    Completion,
    /// Request native cancellation.
    Cancellation,
}

/// Sanitized errors from the native boundary, not Entra provider error mapping.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BrokerError {
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
