// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::NotebookRunRequest;
use crate::run::{
    RunError, RunFailure, RunHandle, RunInput, RunOutcome, RunSession, RunTargetKind,
    SchedulerObserver,
};

fn failure(session: &RunSession<'_>, source: RunError) -> Box<RunFailure> {
    Box::new(RunFailure {
        source,
        evidence: session.evidence().clone(),
    })
}

/// Submit one native whole-Notebook execution and optionally observe its scheduler.
///
/// The caller supplies the already validated plan, original context, executor,
/// deadline and interruption signal through the shared session. This method
/// never uses the legacy `RunNotebook` payload, retries submission, cancels
/// remote work, or interprets scheduler completion as an application exit value.
///
/// # Errors
/// Retains bounded response and mutation evidence on every phase failure.
/// A generic-operation Location is retained as an operation, never guessed to be
/// a scheduler run. Waiting on that unreleased conversion is unsupported.
pub async fn execute(session: &mut RunSession<'_>) -> Result<(), Box<RunFailure>> {
    if session.plan().target().kind() != RunTargetKind::Notebook {
        return Err(failure(session, RunError::WrongOwner));
    }
    let request = NotebookRunRequest::from_input(match session.plan().input() {
        RunInput::Omitted => None,
        RunInput::Json(value) => Some(value.clone()),
    })
    .map_err(|_| failure(session, RunError::InvalidInput))?;
    request
        .validate_standard_execution()
        .map_err(|_| failure(session, RunError::UnsupportedCapability))?;
    let body = request
        .body()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| failure(session, RunError::InvalidInput))?
        .unwrap_or_default();
    let target = session.plan().target();
    let url = session
        .evidence()
        .endpoint
        .url(&format!(
            "/v1/workspaces/{}/notebooks/{}/jobs/execute/instances?beta=false",
            target.owner().workspace(),
            target.owner().item(),
        ))
        .map_err(|_| failure(session, RunError::Context))?;
    let response = session.submit(url, body).await?;
    if response.status != 202 {
        return Err(failure(session, RunError::Protocol));
    }
    let mut locations = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("location"));
    let location = locations
        .next()
        .map(|(_, value)| value)
        .ok_or_else(|| failure(session, RunError::Protocol))?;
    if locations.next().is_some() {
        return Err(failure(session, RunError::Protocol));
    }
    let target = session.plan().target();
    let endpoint = &session.evidence().endpoint;
    let handle = RunHandle::notebook_acceptance(target, endpoint, location)
        .map_err(|error| failure(session, error))?;
    session.bind_handle(&handle)?;
    session.record(RunOutcome::Accepted, None);
    if session.plan().wait() {
        if matches!(handle.kind(), crate::run::RunHandleKind::Operation(_)) {
            return Err(failure(session, RunError::UnsupportedCapability));
        }
        session.observe(&handle, &SchedulerObserver).await?;
    }
    Ok(())
}
