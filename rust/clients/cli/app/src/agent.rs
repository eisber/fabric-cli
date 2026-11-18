// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Best-effort inherited hints, never authenticated agent/session provenance.

use crate::Host;
use fabric_cli_command_contracts::CommandError;
use fabric_core::{AgentKind, RequestCommand, RequestMetadata, diagnostics::RequestId};

pub(crate) fn request_metadata(
    host: &dyn Host,
    skill: Option<&str>,
) -> Result<RequestMetadata, CommandError> {
    use AgentKind::*;
    // ID candidates and presence flags have different roles. In particular a
    // job ID, PID, or "1" marker must never be treated as a conversation ID.
    const SIGNALS: &[(&str, AgentKind, bool)] = &[
        ("COPILOT_AGENT_SESSION_ID", Copilot, true),
        ("COPILOT_CLI", Copilot, false),
        ("COPILOT_AGENT", Copilot, false),
        ("COPILOT_AGENT_JOB_ID", Copilot, false),
        ("CODEX_THREAD_ID", Codex, true),
        ("CODEX_SESSION_ID", Codex, true),
        ("CODEX_CI", Codex, false),
        ("CODEX_SANDBOX", Codex, false),
        ("CLAUDE_CODE_SESSION_ID", Claude, true),
        ("CLAUDECODE", Claude, false),
        ("CLAUDE_CODE_CHILD_SESSION", Claude, false),
        ("CURSOR_AGENT", Cursor, false),
        ("CURSOR_SANDBOX", Cursor, false),
        ("GEMINI_SESSION_ID", Gemini, true),
        ("GEMINI_CLI", Gemini, false),
        ("OPENCODE", OpenCode, false),
        ("OPENCODE_PID", OpenCode, false),
        ("AMP_CURRENT_THREAD_ID", Amp, true),
        ("KIRO_SESSION_ID", Kiro, true),
    ];
    let mut agent = None;
    let mut agent_conflict = false;
    let mut session = None;
    let mut session_conflict = false;
    for &(name, kind, is_id) in SIGNALS {
        let Some(value) = host.environment(name) else {
            continue;
        };
        let Some(value) = value.to_str().filter(|value| {
            !value.trim().is_empty() && *value != "0" && !value.eq_ignore_ascii_case("false")
        }) else {
            continue;
        };
        if agent.is_some_and(|existing| existing != kind) {
            agent_conflict = true;
        }
        agent = Some(kind);
        if is_id && let Some(id) = RequestId::parse(value) {
            if session.is_some_and(|existing| existing != id) {
                session_conflict = true;
            }
            session = Some(id);
        }
    }
    RequestMetadata::new(
        RequestCommand::Api,
        agent.filter(|_| !agent_conflict),
        session.filter(|_| !session_conflict),
        skill,
    )
    .map_err(Into::into)
}
