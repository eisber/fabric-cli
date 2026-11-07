// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded GitHub Releases updates with caller-pinned Ed25519 trust.
//!
//! Defaults have no publisher, credential discovery, network client, scheduler,
//! process launcher, runtime, or settings store. The optional bounded helper is
//! an installed CLI component, never an SDK or downloaded-code boundary. See the
//! crate README for its pending/commit/reconciliation and trust contracts.

#![forbid(unsafe_code)]
#![cfg_attr(test, deny(unnameable_test_items))]

#[cfg(feature = "bounded-helper")]
mod custody;
mod engine;
#[cfg(feature = "bounded-helper")]
mod helper;
mod install;
mod model;
mod policy;
mod protocol;
#[cfg(feature = "bounded-helper")]
mod running;

#[cfg(feature = "bounded-helper")]
pub use custody::{CustodyReceipt, ReceiptBinding, RollbackImage, helper_name};
pub use engine::{
    CheckedUpdate, DownloadedUpdate, HttpRequest, HttpResponse, HttpTransport, InstallContext,
    InstallReceipt, Installer, ResponseSink, UpdateEngine, is_allowed_release_cdn,
};
#[cfg(feature = "fixture-harness")]
pub use helper::run_helper_with_fault;
#[cfg(feature = "bounded-helper")]
pub use helper::{HelperPolicy, HelperResult, HelperState, run_helper};
pub use install::{FileInstall, FileInstallReceipt, FileInstaller};
pub use model::{
    Channel, Control, Error, Identity, InstallCustody, Limits, ReleaseSelector, Repository,
    Snapshot, Target, Trust,
};
pub use policy::{AutomaticContext, CheckSchedule, Policy, StartupDecision};
pub use protocol::{Manifest, VerificationContext, VerifiedManifest, verify_manifest};
#[cfg(feature = "bounded-helper")]
pub use running::{
    ExitAcknowledgement, ParentLifetime, PendingUpdate, RunningInstaller, reconcile,
};
