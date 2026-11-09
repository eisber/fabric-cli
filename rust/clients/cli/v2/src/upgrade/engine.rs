// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The production apply core, with injected transport and installation custody.
//! Offline tests use the real signed engine here without executing a helper.

use fabric_cli_update::{
    CheckedUpdate, DownloadedUpdate, HttpTransport, ReleaseSelector, UpdateEngine,
};

use super::{UpgradeExecutor, UpgradeObservation, UpgradeRequest, execute, native::failure};
use crate::{CliError, Control, OperationResult};

/// Trusted installation boundary; downloading never establishes custody.
pub trait Installation {
    /// Validate custody or observe only the exact existing transaction.
    fn preflight(&mut self) -> Result<Option<UpgradeObservation>, CliError>;
    /// Queue already verified bytes without claiming helper completion.
    fn queue(&mut self, update: DownloadedUpdate) -> Result<UpgradeObservation, CliError>;
}

/// Apply using the same real engine sequence as the native production session.
///
/// The caller supplies independently pinned engine identity and trust.
/// Transport and installation injection never changes production argument
/// behavior or grants a signing key, installation owner or helper authority.
pub fn apply(
    request: &UpgradeRequest,
    engine: &UpdateEngine,
    transport: &mut impl HttpTransport,
    installation: &mut impl Installation,
    control: &(impl Control + fabric_cli_update::Control),
) -> Result<OperationResult, CliError> {
    execute(
        request,
        &mut Executor {
            engine,
            transport,
            installation,
            control,
        },
        control,
    )
}

struct Executor<'a, T, I, C> {
    engine: &'a UpdateEngine,
    transport: &'a mut T,
    installation: &'a mut I,
    control: &'a C,
}

impl<T, I, C> UpgradeExecutor for Executor<'_, T, I, C>
where
    T: HttpTransport,
    I: Installation,
    C: fabric_cli_update::Control,
{
    type Checked = CheckedUpdate;
    type Downloaded = DownloadedUpdate;

    fn preflight(&mut self) -> Result<Option<UpgradeObservation>, CliError> {
        self.installation.preflight()
    }

    fn check(&mut self, tag: Option<&str>) -> Result<Self::Checked, CliError> {
        let selector = tag
            .map(ReleaseSelector::exact_tag)
            .transpose()
            .map_err(failure)?
            .unwrap_or(ReleaseSelector::LatestRustStable);
        self.engine
            .check(&selector, self.transport, self.control)
            .map_err(failure)
    }

    fn requires_major(&self, checked: &Self::Checked) -> bool {
        checked.requires_major_opt_in()
    }

    fn download(&mut self, checked: Self::Checked) -> Result<Self::Downloaded, CliError> {
        self.engine
            .download(checked, self.transport, self.control)
            .map_err(failure)
    }

    fn queue(&mut self, update: Self::Downloaded) -> Result<UpgradeObservation, CliError> {
        self.installation.queue(update)
    }
}
