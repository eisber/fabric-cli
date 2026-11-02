// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Interactive-only subset of the recovered draft's caller-owned session state.

#[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
use fabric_cli_context::{Element, HomeContext, PathElement};
#[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
use fabric_cli_local::{LocalError, NavigationState};
use std::process::ExitCode;
#[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    Returned(u8),
    Stop(u8),
    Cancelled(u8),
}

impl CommandOutcome {
    pub(crate) fn from_exit(code: ExitCode) -> Self {
        Self::Returned(
            (0..=u8::MAX)
                .find(|exit| code == ExitCode::from(*exit))
                .unwrap_or(1),
        )
    }

    pub(crate) fn exit_code(self) -> ExitCode {
        ExitCode::from(self.exit())
    }

    pub(crate) fn exit(self) -> u8 {
        match self {
            Self::Returned(exit) | Self::Stop(exit) | Self::Cancelled(exit) => exit,
        }
    }

    #[cfg(any(feature = "repl", feature = "native-read", feature = "windows-broker"))]
    pub(crate) fn from_code(code: Option<&str>, exit: u8) -> Self {
        match code {
            Some(
                "cancelled" | "Cancelled" | "OperationCancelled" | "LongRunningOperationCancelled",
            ) => Self::Cancelled(exit),
            Some("UnexpectedError") => Self::Stop(exit),
            _ => Self::Returned(exit),
        }
    }
}

#[cfg(all(feature = "native-read", not(feature = "repl"), not(windows)))]
pub(crate) struct ReplState;

#[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
pub(crate) struct ReplState {
    initial: Result<(String, bool), fabric_cli_output::CommandError>,
    pub(crate) navigation: Option<NavigationState>,
    #[cfg(all(feature = "native-read", windows))]
    binding: Option<(String, String, String)>,
    #[cfg(all(feature = "native-read", windows))]
    native_navigation: bool,
}

#[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
impl ReplState {
    #[cfg(feature = "repl")]
    pub(crate) fn capture_interactive() -> Self {
        Self::new_interactive(crate::local_host::context_selection_for(false))
    }

    #[cfg(feature = "repl")]
    pub(crate) fn new_interactive(initial: Result<(String, bool), LocalError>) -> Self {
        Self {
            initial: initial.map_err(|error| error.command_error()),
            navigation: None,
            #[cfg(all(feature = "native-read", windows))]
            binding: None,
            #[cfg(all(feature = "native-read", windows))]
            native_navigation: false,
        }
    }

    #[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
    pub(crate) fn path(&self) -> Result<String, LocalError> {
        self.initial
            .as_ref()
            .map(|(path, _)| path.clone())
            .map_err(captured_error)
    }

    pub(crate) fn commit_navigation(&mut self, before: &Arc<PathElement>) {
        if let Some(navigation) = &self.navigation
            && !Arc::ptr_eq(before, navigation.current())
        {
            self.initial = Ok((navigation.current().path(), false));
        }
    }

    #[cfg(feature = "repl")]
    pub(crate) fn initialize_local(&mut self) -> Result<(), LocalError> {
        if self.navigation.is_none() {
            self.navigation =
                Some(self.navigation(Element::tenant("", ""), HomeContext::Unspecified)?);
        }
        Ok(())
    }

    #[cfg(all(feature = "native-read", windows))]
    pub(crate) fn bind(
        &mut self,
        tenant: &str,
        account: &str,
        client: &str,
        home: HomeContext,
    ) -> Result<(), LocalError> {
        let binding = (tenant.to_owned(), account.to_owned(), client.to_owned());
        match &self.binding {
            Some(existing) if existing != &binding => {
                return Err(LocalError::command(
                    "InvalidInput",
                    "The interactive credential selection cannot change",
                ));
            }
            Some(_) if self.native_navigation => return Ok(()),
            Some(_) | None => {}
        }
        self.navigation = Some(self.navigation(Element::tenant(tenant, tenant), home)?);
        self.binding = Some(binding);
        self.native_navigation = true;
        Ok(())
    }

    #[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
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
            false,
        )
    }
}

#[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
fn captured_error(error: &fabric_cli_output::CommandError) -> LocalError {
    let envelope = error.envelope(
        fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z").expect("constant timestamp"),
        None,
    );
    LocalError::Context(fabric_cli_context::ContextError::lookup(
        envelope.result().error_code().unwrap_or("UnknownError"),
        envelope
            .result()
            .message()
            .unwrap_or("Failed to load command context"),
    ))
}

#[cfg(any(feature = "repl", all(feature = "native-read", windows)))]
pub(crate) fn error_outcome(error: &fabric_cli_output::CommandError, exit: u8) -> CommandOutcome {
    let envelope = error.envelope(
        fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z").expect("constant timestamp"),
        None,
    );
    CommandOutcome::from_code(envelope.result().error_code(), exit)
}

#[cfg(all(feature = "native-read", windows))]
pub(crate) fn family_outcome(output: &fabric_cli::families::FamilyOutput) -> CommandOutcome {
    use fabric_cli::families::FamilyEvidence;
    let exit = output.plan.exit_code;
    match &output.evidence {
        FamilyEvidence::CommandFailure(error) => error_outcome(error, exit),
        FamilyEvidence::Access(Err(error)) => CommandOutcome::from_code(Some(&error.code), exit),
        FamilyEvidence::Resource(Err(error)) => CommandOutcome::from_code(Some(&error.code), exit),
        FamilyEvidence::Removal(Err(error)) => {
            CommandOutcome::from_code(Some(&error.error.code), exit)
        }
        FamilyEvidence::Storage(Err(error)) => {
            CommandOutcome::from_code(error.output().error_code.as_deref(), exit)
        }
        FamilyEvidence::ResourceFiles(Err(error)) => {
            CommandOutcome::from_code(error.output().error_code.as_deref(), exit)
        }
        FamilyEvidence::ResourceFilesRenderFailure { .. } => CommandOutcome::Stop(exit),
        FamilyEvidence::Transfer(Err(error)) => CommandOutcome::from_code(
            error
                .envelope(
                    fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z")
                        .expect("constant timestamp"),
                    None,
                )
                .result()
                .error_code(),
            exit,
        ),
        FamilyEvidence::Table(evidence) => match &evidence.result {
            Err(error) => CommandOutcome::from_code(
                error
                    .envelope(
                        fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z")
                            .expect("constant timestamp"),
                        None,
                    )
                    .result()
                    .error_code(),
                exit,
            ),
            Ok(_) => CommandOutcome::Returned(exit),
        },
        FamilyEvidence::Deploy(Err(error)) => {
            use fabric_cli::deploy::DeployFailure;
            match &error.failure {
                DeployFailure::Command(error) | DeployFailure::Output { error, .. } => {
                    error_outcome(error, exit)
                }
                _ => CommandOutcome::Returned(exit),
            }
        }

        _ => CommandOutcome::Returned(exit),
    }
}

#[cfg(all(test, feature = "repl"))]
#[path = "repl_state_tests.rs"]
mod tests;
