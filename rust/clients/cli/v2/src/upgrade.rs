// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Apply-only integration with the existing signed Rust updater.
//!
//! The selected installation is the actual running executable. No configuration,
//! credential, PATH, release URL, runtime key or helper discovery is performed.
//! `UpgradeSession` belongs to outermost main through output and exit.
//! A pending helper is not an installed or restarted CLI.
//!
//! Production builds must independently supply `FAB_CLI_V2_RELEASE_PUBLIC_KEY`,
//! `FAB_CLI_V2_RELEASE_CHANNEL`, `FAB_CLI_V2_RELEASE_VARIANT`,
//! `FAB_CLI_V2_INSTALL_CUSTODY=standalone` and an absolute build-time
//! `FAB_CLI_V2_INSTALL_RECEIPT`. The latter identifies the trusted installer's
//! revision-2 sidecar, not user configuration. The installed helper must be built
//! with the same key/channel/variant through its existing `FAB_RELEASE_*` pins.
//! The family remains `microsoft/fabric-cli`, product `fab-rs`, `rust-vSEMVER`;
//! the mandatory distinct `cli-v2+self-update[+windows-broker]` variant prevents
//! replacing this executable with a v1 asset. Missing pins fail before HTTP.
//! Native signing, publisher approval and health rollback remain release gates.

#![cfg_attr(test, deny(unnameable_test_items))]

use serde_json::json;

use crate::{CliError, Control, EffectKind, EffectState, OperationResult, Status, check_control};

#[cfg(feature = "self-update")]
pub mod engine;
#[cfg(feature = "self-update")]
mod native;
#[cfg(feature = "self-update")]
mod transport;

/// Explicit apply request; there is no inspection or cross-major override.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpgradeRequest {
    /// Canonical SemVer, mapped exactly to `rust-vVERSION`.
    pub target_version: Option<String>,
}

/// Main-frame lifetime capability; keep alive through all diagnostics and exit.
///
/// Never put this in a temporary expression around `run`. Once pending, finish
/// output and exit without dispatching or replaying another command.
#[derive(Default)]
pub struct UpgradeSession {
    #[cfg(feature = "self-update")]
    parent: fabric_cli_update::ParentLifetime,
}

impl UpgradeSession {
    /// Construct without filesystem, network or process effects.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply through the trusted native adapter, never inspection-as-upgrade.
    ///
    /// Retain this session in outermost main, including every output-failure
    /// path. This method does not print, exit, replay or start the new image.
    pub fn run(&mut self, request: &UpgradeRequest) -> Result<OperationResult, CliError> {
        let _ = target_tag(request)?;
        #[cfg(feature = "self-update")]
        {
            native::run(request, self)
        }
        #[cfg(not(feature = "self-update"))]
        {
            Err(CliError::new(
                "UnsupportedCapability",
                "This CLI v2 build has no self-update host. No inspection, download or installation was substituted.",
            ))
        }
    }

    /// Whether this frame retains an independently verified helper connection.
    pub fn is_pending(&self) -> bool {
        #[cfg(feature = "self-update")]
        {
            self.parent.is_pending()
        }
        #[cfg(not(feature = "self-update"))]
        {
            false
        }
    }

    /// Request cancellation without releasing the parent lifetime capability.
    ///
    /// A failed cancellation send is unknown, not proof installation stopped.
    pub fn cancel(&mut self) -> Result<(), CliError> {
        #[cfg(feature = "self-update")]
        {
            self.parent.cancel().map_err(native::failure)
        }
        #[cfg(not(feature = "self-update"))]
        {
            Ok(())
        }
    }
}

/// Validate without host effects and return the exact Rust release tag.
pub fn target_tag(request: &UpgradeRequest) -> Result<Option<String>, CliError> {
    request
        .target_version
        .as_ref()
        .map(|value| {
            if value.len() > 128 {
                return Err(invalid_version());
            }
            let version = semver::Version::parse(value).map_err(|_| invalid_version())?;
            if version.to_string() != *value {
                return Err(invalid_version());
            }
            Ok(format!("rust-v{version}"))
        })
        .transpose()
}

fn invalid_version() -> CliError {
    CliError::new(
        "InvalidInput",
        "--target-version requires canonical SemVer, not a tag, URL or version range.",
    )
}

/// Final observation from a trusted executor, not an inferred downstream state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeObservation {
    /// A helper accepted work, or an exact prior helper is still active.
    Pending {
        /// None when an active prior transaction's candidate is not observed.
        candidate_version: Option<String>,
    },
    /// Exact prior transaction reconciliation returned these local facts.
    Reconciled {
        /// Signed candidate version; not necessarily the installed version.
        candidate_version: String,
        /// Whether candidate bytes are actually present.
        installed: bool,
        /// Whether the installer's version/digest record was advanced.
        receipt_committed: bool,
        /// Whether exact-owned recovery evidence must remain.
        recovery_required: bool,
    },
    /// Replacement may have occurred but completion was not established.
    Unknown,
}

/// Injectable apply orchestration boundary for offline tests and trusted hosts.
///
/// Checked and downloaded values are deliberately different associated types.
/// An implementation must not return an installed observation for either one.
/// Production uses the existing updater's unforgeable checked/downloaded values.
pub trait UpgradeExecutor {
    /// Verified metadata, not executable bytes or installation.
    type Checked;
    /// Verified bytes, not installation.
    type Downloaded;

    /// Verify trust/custody and reconcile only an exact prior transaction.
    fn preflight(&mut self) -> Result<Option<UpgradeObservation>, CliError>;
    /// Check exactly the selected release using pinned trust.
    fn check(&mut self, tag: Option<&str>) -> Result<Self::Checked, CliError>;
    /// Report whether metadata crosses the actual current major version.
    fn requires_major(&self, checked: &Self::Checked) -> bool;
    /// Consume metadata and verify the corresponding bounded executable bytes.
    fn download(&mut self, checked: Self::Checked) -> Result<Self::Downloaded, CliError>;
    /// Queue the installed helper; preserve lifetime on every post-ready failure.
    fn queue(&mut self, downloaded: Self::Downloaded) -> Result<UpgradeObservation, CliError>;
}

/// Run the same apply-only sequence with an injected executor and cancellation.
///
/// No fake transport, installer, signing key or success path is selected by
/// production arguments. This boundary does not grant installation authority.
pub fn execute(
    request: &UpgradeRequest,
    executor: &mut impl UpgradeExecutor,
    control: &impl Control,
) -> Result<OperationResult, CliError> {
    let tag = target_tag(request)?;
    check_control(control)?;
    if let Some(observation) = executor.preflight()? {
        return Ok(observed(observation, request));
    }
    check_control(control)?;
    let checked = executor.check(tag.as_deref())?;
    if executor.requires_major(&checked) {
        return Err(CliError::new(
            "UpgradeMajorVersion",
            "CLI v2 upgrade permits the current major version only; selecting a target version is not a major-upgrade override.",
        ));
    }
    check_control(control)?;
    let downloaded = executor.download(checked)?;
    check_control(control)?;
    executor
        .queue(downloaded)
        .map(|observation| observed(observation, request))
}

fn observed(observation: UpgradeObservation, request: &UpgradeRequest) -> OperationResult {
    let (status, data, effect) = match observation {
        UpgradeObservation::Pending { candidate_version } => (
            Status::Accepted,
            json!({
                "state": if candidate_version.is_some() { "pending_parent_exit" } else { "pending" },
                "candidate_version": candidate_version,
                "installed": if candidate_version.is_some() { Some(false) } else { None },
                "receipt_committed": if candidate_version.is_some() { Some(false) } else { None },
                "completion_confirmed": false,
                "exit_required": true,
                "restart_confirmed": false,
                "health_verified": false
            }),
            EffectState::Pending,
        ),
        UpgradeObservation::Reconciled {
            candidate_version,
            installed,
            receipt_committed,
            recovery_required,
        } => (
            if installed
                && receipt_committed
                && !recovery_required
                && request
                    .target_version
                    .as_ref()
                    .is_none_or(|target| *target == candidate_version)
            {
                Status::Succeeded
            } else {
                Status::Partial
            },
            json!({
                "state": "reconciled",
                "candidate_version": candidate_version,
                "requested_target_version": request.target_version,
                "reconciled_prior_transaction": true,
                "release_check_performed": false,
                "installed": installed,
                "receipt_committed": receipt_committed,
                "recovery_required": recovery_required,
                "completion_confirmed": installed && receipt_committed && !recovery_required,
                "exit_required": true,
                "restart_confirmed": false,
                "health_verified": false
            }),
            if recovery_required {
                EffectState::Unknown
            } else if installed {
                EffectState::Completed
            } else {
                EffectState::NotStarted
            },
        ),
        UpgradeObservation::Unknown => (
            Status::Unknown,
            json!({
                "state": "unknown",
                "installed": null,
                "receipt_committed": null,
                "completion_confirmed": false,
                "recovery_required": true,
                "exit_required": true,
                "restart_confirmed": false,
                "health_verified": false
            }),
            EffectState::Unknown,
        ),
    };
    OperationResult::new(status, data).with_effect(EffectKind::Installation, effect)
}
