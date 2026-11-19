// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Binary-private, caller-owned context. Persistence follows the selected runtime and settings.

use fabric_cli::parser::batch::CommandOutcome as BatchOutcome;
use fabric_cli_context::{Element, HomeContext, PathElement};
use fabric_cli_local::{LocalError, NavigationState};
use std::sync::Arc;

/// Binary-only control reason. Batch APIs retain their existing coarse outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    Returned(u8),
    Stop(u8),
    Cancelled(u8),
}

impl From<CommandOutcome> for BatchOutcome {
    fn from(outcome: CommandOutcome) -> Self {
        match outcome {
            CommandOutcome::Returned(exit) => Self::Returned(exit),
            CommandOutcome::Stop(exit) | CommandOutcome::Cancelled(exit) => Self::Stop(exit),
        }
    }
}

impl From<BatchOutcome> for CommandOutcome {
    fn from(outcome: BatchOutcome) -> Self {
        match outcome {
            BatchOutcome::Returned(exit) => Self::Returned(exit),
            BatchOutcome::Stop(exit) => Self::Stop(exit),
        }
    }
}

pub(crate) struct BatchState {
    #[cfg(feature = "docs-search")]
    pending_capture: bool,
    interactive: bool,
    initial: Result<(String, bool), fabric_cli_output::CommandError>,
    pub(crate) navigation: Option<NavigationState>,
    #[cfg(all(
        feature = "native-host",
        any(windows, feature = "headless", feature = "interactive", test)
    ))]
    binding: Option<(String, String, String, String)>,
    #[cfg(all(
        feature = "native-host",
        any(windows, feature = "headless", feature = "interactive")
    ))]
    home: Result<Option<String>, String>,
}

impl BatchState {
    pub(crate) fn capture() -> Self {
        Self::new(crate::local_host::context_selection())
    }

    #[cfg(feature = "docs-search")]
    pub(crate) fn capture_deferred() -> Self {
        let mut state = Self::new(Ok(("/".into(), false)));
        state.pending_capture = true;
        state
    }

    #[cfg(feature = "docs-search")]
    pub(crate) fn capture_if_needed(&mut self) {
        if self.pending_capture {
            self.initial =
                crate::local_host::context_selection().map_err(|error| error.command_error());
            self.pending_capture = false;
        }
    }

    #[cfg(feature = "docs-search")]
    pub(crate) fn is_deferred(&self) -> bool {
        self.pending_capture
    }

    #[cfg(feature = "repl")]
    pub(crate) fn capture_interactive() -> Self {
        Self::new_interactive(crate::local_host::context_selection_for(false))
    }

    #[cfg(feature = "repl")]
    pub(crate) fn new_interactive(initial: Result<(String, bool), LocalError>) -> Self {
        let mut state = Self::new(initial);
        state.interactive = true;
        state
    }

    fn new(initial: Result<(String, bool), LocalError>) -> Self {
        Self {
            #[cfg(feature = "docs-search")]
            pending_capture: false,
            interactive: false,
            initial: initial.map_err(|error| error.command_error()),
            navigation: None,
            #[cfg(all(
                feature = "native-host",
                any(windows, feature = "headless", feature = "interactive", test)
            ))]
            binding: None,
            #[cfg(all(
                feature = "native-host",
                any(windows, feature = "headless", feature = "interactive")
            ))]
            home: match std::env::var("FAB_RUST_HOME") {
                Ok(home) => Ok(Some(home)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(_) => Err("Invalid home selector.".into()),
            },
        }
    }

    pub(crate) fn path(&self) -> Result<String, LocalError> {
        self.initial
            .as_ref()
            .map(|(path, _)| path.clone())
            .map_err(captured_error)
    }

    pub(crate) fn take_restored(&mut self) -> Result<bool, LocalError> {
        let (_, restored) = self
            .initial
            .as_mut()
            .map_err(|error| captured_error(error))?;
        Ok(std::mem::take(restored))
    }

    pub(crate) fn commit_navigation(&mut self, before: &Arc<PathElement>) {
        if let Some(navigation) = &self.navigation
            && !Arc::ptr_eq(before, navigation.current())
        {
            let restored = self.initial.as_ref().is_ok_and(|(_, restored)| *restored);
            self.initial = Ok((navigation.current().path(), restored));
        }
    }

    pub(crate) fn initialize_local(&mut self) -> Result<(), LocalError> {
        if self.navigation.is_none() {
            self.navigation =
                Some(self.navigation(Element::tenant("", ""), HomeContext::Unspecified)?);
        }
        Ok(())
    }

    fn navigation(
        &self,
        tenant: Arc<Element>,
        home: HomeContext,
    ) -> Result<NavigationState, LocalError> {
        NavigationState::new(
            PathElement::from_fabric(tenant),
            home,
            std::env::current_dir()?.to_string_lossy().into_owned(),
            crate::local_host::session_id()?,
            !self.interactive,
        )
    }

    #[cfg(all(
        feature = "native-host",
        any(windows, feature = "headless", feature = "interactive", test)
    ))]
    pub(crate) async fn family<T>(
        &mut self,
        execute: impl AsyncFnOnce(&mut NavigationState) -> T,
    ) -> T {
        let navigation = self.navigation.as_mut().expect("native context bound");
        let before = Arc::clone(navigation.current());
        let result = execute(navigation).await;
        self.commit_navigation(&before);
        result
    }

    #[cfg(all(
        feature = "native-host",
        any(windows, feature = "headless", feature = "interactive")
    ))]
    pub(crate) fn home(&self) -> Result<Option<String>, String> {
        self.home.clone()
    }

    #[cfg(all(
        feature = "native-host",
        any(windows, feature = "headless", feature = "interactive", test)
    ))]
    pub(crate) fn bind(
        &mut self,
        tenant: &str,
        account: &str,
        client: &str,
        home: HomeContext,
    ) -> Result<(), LocalError> {
        self.bind_provider(tenant, account, client, "broker", home)
    }

    #[cfg(all(
        feature = "native-host",
        any(windows, feature = "headless", feature = "interactive", test)
    ))]
    pub(crate) fn bind_provider(
        &mut self,
        tenant: &str,
        account: &str,
        client: &str,
        provider: &str,
        home: HomeContext,
    ) -> Result<(), LocalError> {
        let binding = (
            tenant.to_owned(),
            account.to_owned(),
            client.to_owned(),
            provider.to_owned(),
        );
        match &self.binding {
            Some(existing) if existing != &binding => {
                return Err(LocalError::command(
                    "InvalidInput",
                    "The batch credential selection cannot change",
                ));
            }
            Some(_) if self.navigation.is_some() => return Ok(()),
            Some(_) | None => {}
        }
        self.navigation = Some(self.navigation(Element::tenant(tenant, tenant), home)?);
        self.binding = Some(binding);
        Ok(())
    }

    #[cfg(all(
        feature = "native-host",
        any(windows, feature = "headless", feature = "interactive", test)
    ))]
    pub(crate) fn bind_read_provider(
        &mut self,
        tenant: &str,
        account: &str,
        client: &str,
        provider: &str,
    ) -> Result<(), LocalError> {
        let binding = (
            tenant.to_owned(),
            account.to_owned(),
            client.to_owned(),
            provider.to_owned(),
        );
        if self
            .binding
            .as_ref()
            .is_some_and(|existing| existing != &binding)
        {
            return Err(LocalError::command(
                "InvalidInput",
                "The batch credential selection cannot change",
            ));
        }
        self.binding = Some(binding);
        Ok(())
    }
}

fn captured_error(error: &fabric_cli_output::CommandError) -> LocalError {
    let envelope = error.envelope(timestamp(), None);
    LocalError::Context(fabric_cli_context::ContextError::lookup(
        envelope.result().error_code().unwrap_or("UnknownError"),
        envelope
            .result()
            .message()
            .unwrap_or("Failed to load command context"),
    ))
}

fn timestamp() -> fabric_cli_output::Timestamp {
    fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z").expect("constant timestamp")
}

pub(crate) fn error_outcome(error: &fabric_cli_output::CommandError, exit: u8) -> CommandOutcome {
    code_outcome(
        error.envelope(timestamp(), None).result().error_code(),
        exit,
    )
}

fn code_outcome(code: Option<&str>, exit: u8) -> CommandOutcome {
    match code {
        Some(
            "cancelled" | "Cancelled" | "OperationCancelled" | "LongRunningOperationCancelled",
        ) => CommandOutcome::Cancelled(exit),
        Some("UnexpectedError") => CommandOutcome::Stop(exit),
        _ => CommandOutcome::Returned(exit),
    }
}

#[cfg(all(
    feature = "native-host",
    any(windows, feature = "headless", feature = "interactive", test)
))]
pub(crate) fn read_outcome(output: &fabric_cli::session::ReadDispatch) -> CommandOutcome {
    use fabric_sdk_operations::{OperationErrorKind, OperationExecutionError};

    let exit = output.plan.exit_code;
    if let Some(error) = &output.operation_error {
        match &error.kind {
            OperationErrorKind::Cancelled
            | OperationErrorKind::Execution(OperationExecutionError::Cancelled) => {
                return CommandOutcome::Cancelled(exit);
            }
            OperationErrorKind::OperationFailed { status, .. } if status == "Cancelled" => {
                return CommandOutcome::Cancelled(exit);
            }
            _ => {}
        }
        let error = fabric_cli_mutations::MutationError::from(error.resource_error());
        return code_outcome(Some(&error.code), exit);
    }
    output
        .error
        .as_ref()
        .map_or(CommandOutcome::Returned(exit), |error| {
            error_outcome(error, exit)
        })
}

#[cfg(all(
    feature = "native-host",
    any(windows, feature = "headless", feature = "interactive", test)
))]
pub(crate) fn job_outcome(output: &fabric_cli::jobs::JobDispatch) -> CommandOutcome {
    let exit = output.plan.exit_code;
    output
        .error
        .as_ref()
        .map_or(CommandOutcome::Returned(exit), |error| {
            code_outcome(
                error.envelope(timestamp(), None).result().error_code(),
                exit,
            )
        })
}

#[cfg(all(
    feature = "native-host",
    any(windows, feature = "headless", feature = "interactive", test)
))]
pub(crate) fn family_outcome(output: &fabric_cli::families::FamilyOutput) -> CommandOutcome {
    use fabric_cli::families::FamilyEvidence;
    let exit = output.plan.exit_code;
    match &output.evidence {
        FamilyEvidence::CommandFailure(error) => error_outcome(error, exit),
        FamilyEvidence::Access(Err(error))
        | FamilyEvidence::AccessTable {
            result: Err(error), ..
        } => code_outcome(Some(&error.code), exit),
        FamilyEvidence::Resource(Err(error)) => code_outcome(Some(&error.code), exit),
        FamilyEvidence::Removal(Err(error)) => code_outcome(Some(&error.error.code), exit),
        FamilyEvidence::Storage(Err(error)) => {
            code_outcome(error.output().error_code.as_deref(), exit)
        }
        FamilyEvidence::ResourceFiles(Err(error)) => {
            code_outcome(error.output().error_code.as_deref(), exit)
        }
        FamilyEvidence::ResourceFilesRenderFailure { .. } => CommandOutcome::Stop(exit),
        FamilyEvidence::Transfer(Err(error)) => code_outcome(
            error.envelope(timestamp(), None).result().error_code(),
            exit,
        ),
        FamilyEvidence::Table(evidence) => match &evidence.result {
            Err(error) => code_outcome(
                error.envelope(timestamp(), None).result().error_code(),
                exit,
            ),
            Ok(_) => code_outcome(None, exit),
        },
        FamilyEvidence::Deploy(Err(error)) => {
            use fabric_cli::deploy::DeployFailure;
            match &error.failure {
                DeployFailure::Command(error) | DeployFailure::Output { error, .. } => {
                    error_outcome(error, exit)
                }
                _ => code_outcome(None, exit),
            }
        }
        _ => code_outcome(None, exit),
    }
}

#[cfg(test)]
#[path = "batch_state_tests.rs"]
mod tests;
