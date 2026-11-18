// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    Host,
    console::{ConsoleConfirmation, ConsoleProgress},
    definition, resources,
};
use async_trait::async_trait;
use fabric_cli_api::ApiReporting;
use fabric_cli_command_contracts::{
    CommandDiagnostics, CommandError, CommandOutput, ValidatedInvocation, selected,
};
use fabric_core::{
    Audience, CancellationToken, Clock, CoreError, OperationContext, OperationKind, RecoveryInfo,
    RequestCommand,
    diagnostics::{DiagnosticEvent, DiagnosticObserver, DiagnosticPhase, DiagnosticStage},
};
use fabric_sdk_http::HttpClient;
use fabric_sdk_operations::{PollObserver, PollProgress};
use std::{sync::Arc, time::Duration};

pub(super) async fn execute(
    invocation: &ValidatedInvocation,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &Arc<ConsoleProgress>,
    confirmer: &ConsoleConfirmation,
) -> Result<CommandOutput, CommandError> {
    match invocation.matches().subcommand_name() {
        Some("auth") => {
            fabric_cli_auth::execute(invocation, host, cancellation, observer.as_ref()).await
        }
        Some("api") => {
            let reporting = Arc::new(ApiConsole(observer.clone()));
            fabric_cli_api::execute(invocation, host, cancellation, &reporting).await
        }
        Some(kind @ ("workspace" | "item")) => {
            let kind = if kind == "workspace" {
                "workspace"
            } else {
                "item"
            };
            let args = invocation.command(kind)?;
            let started = observer.start(DiagnosticStage::Prepare).await?;
            let prepared = (|| {
                let prepared = resources::prepare(kind, args, &|| {
                    definition::home_from_env(cfg!(windows), |name| host.environment(name))
                })?;
                let endpoint = fabric_cli_api::resolve_endpoint(host, Audience::Fabric)?;
                let metadata = fabric_cli_api::request_metadata(
                    host,
                    args.get_one::<String>("skill").map(String::as_str),
                    RequestCommand::Http,
                )?;
                Ok((prepared, endpoint, metadata))
            })();
            let (prepared, endpoint, metadata) = observer
                .finish(DiagnosticStage::Prepare, started, prepared)
                .await?;
            let started = observer.start(DiagnosticStage::Selection).await?;
            let binding = observer
                .finish(DiagnosticStage::Selection, started, selected(host))
                .await?;
            let clock = host.clock();
            observer.set_poll_clock(clock.clone())?;
            let started = observer.start(DiagnosticStage::Authentication).await?;
            let credential = observer
                .finish(
                    DiagnosticStage::Authentication,
                    started,
                    host.credential(binding),
                )
                .await?;
            let mut client = HttpClient::new(credential, host.transport()?, clock.clone())
                .with_request_metadata(metadata)
                .with_endpoint(endpoint.clone());
            if observer.diagnostics_enabled() {
                client = client.with_diagnostics(observer.clone());
            }
            let context =
                OperationContext::new(clock.now(), Duration::from_secs(240), cancellation)?;
            resources::execute(
                prepared, &client, clock, &endpoint, &context, observer, confirmer,
            )
            .await
            .map(CommandOutput::Resource)
        }
        _ => Err(CommandError::new(
            "InvalidInput",
            "A command is required; run fab-rs --help to list commands",
        )),
    }
}

struct ApiConsole(Arc<ConsoleProgress>);

impl CommandDiagnostics for ApiConsole {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    async fn stage(
        &self,
        stage: DiagnosticStage,
        phase: DiagnosticPhase,
        duration: Duration,
    ) -> Result<(), CommandError> {
        self.0.stage(stage, phase, duration).await
    }
}

#[async_trait]
impl DiagnosticObserver for ApiConsole {
    async fn observe(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        DiagnosticObserver::observe(self.0.as_ref(), event).await
    }
}

#[async_trait]
impl PollObserver for ApiConsole {
    async fn accepted(&self, kind: OperationKind, status: u16) -> Result<(), CoreError> {
        self.0.accepted(kind, status).await
    }

    async fn waiting(&self, kind: OperationKind, delay: Duration) -> Result<(), CoreError> {
        self.0.waiting(kind, delay).await
    }

    async fn polling(&self, kind: OperationKind) -> Result<(), CoreError> {
        self.0.polling(kind).await
    }

    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError> {
        PollObserver::observe(self.0.as_ref(), progress).await
    }

    async fn diagnostic(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        self.0.diagnostic(event).await
    }
}

impl ApiReporting for ApiConsole {
    fn set_poll_clock(&self, clock: Arc<dyn Clock>) -> Result<(), CoreError> {
        self.0.set_poll_clock(clock)
    }

    fn diagnostics_enabled(&self) -> bool {
        self.0.diagnostics_enabled()
    }

    fn set_recovery(&self, recovery: Option<RecoveryInfo>) {
        self.0.set_recovery(recovery);
    }

    fn recovery(&self) -> Option<RecoveryInfo> {
        self.0.recovery()
    }

    async fn progress_failure(&self) -> CoreError {
        self.0.progress_failure().await
    }

    async fn finish_progress(&self) -> Result<(), CoreError> {
        self.0.finish_progress().await
    }
}
