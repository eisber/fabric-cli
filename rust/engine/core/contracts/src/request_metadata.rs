// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated, explicitly injected request identification; never ambient discovery.

use crate::{Audience, CoreError, ErrorKind, diagnostics::RequestId};
use std::{collections::BTreeMap, fmt};

/// Executor-owned User-Agent field.
pub const USER_AGENT_HEADER: &str = "user-agent";
/// Executor-owned correlation field; a session ID, not an idempotency key.
pub const CLIENT_REQUEST_ID_HEADER: &str = "x-ms-client-request-id";
/// Executor-owned Fabric skill field.
pub const FABRIC_SKILL_HEADER: &str = "x-ms-fabric-skill";

/// Fixed command labels safe to include in a User-Agent.
#[derive(Clone, Copy)]
pub enum RequestCommand {
    /// Direct engine use, outside a CLI command.
    Http,
    /// Raw API command.
    Api,
}

/// Recognized agent families; raw environment values never become labels.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum AgentKind {
    /// GitHub Copilot.
    Copilot,
    /// OpenAI Codex.
    Codex,
    /// Claude Code.
    Claude,
    /// Cursor.
    Cursor,
    /// Gemini CLI.
    Gemini,
    /// OpenCode.
    OpenCode,
    /// Amp.
    Amp,
    /// Kiro.
    Kiro,
}
impl AgentKind {
    fn label(self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::Codex => "codex",
            Self::Claude => "claude-code",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini-cli",
            Self::OpenCode => "opencode",
            Self::Amp => "amp",
            Self::Kiro => "kiro",
        }
    }
}

/// Invocation-scoped metadata. Debug deliberately excludes skill and session values.
#[derive(Clone)]
pub struct RequestMetadata {
    user_agent: String,
    session_id: Option<RequestId>,
    skill: Option<String>,
}
impl fmt::Debug for RequestMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestMetadata").finish_non_exhaustive()
    }
}
impl Default for RequestMetadata {
    fn default() -> Self {
        Self::new(RequestCommand::Http, None, None, None)
            .expect("static default request metadata is valid")
    }
}
impl RequestMetadata {
    /// Construct bounded metadata from a fixed command/agent and an optional parsed GUID.
    ///
    /// Skills must be printable ASCII, at most 1024 bytes. Empty or space-only
    /// values are omitted; nonempty values are preserved verbatim.
    pub fn new(
        command: RequestCommand,
        agent: Option<AgentKind>,
        session_id: Option<RequestId>,
        skill: Option<&str>,
    ) -> Result<Self, CoreError> {
        if skill.is_some_and(|value| {
            value.len() > 1024 || value.bytes().any(|byte| !(32..=126).contains(&byte))
        }) {
            return Err(CoreError::new(
                ErrorKind::InvalidInput,
                "Skill metadata must be at most 1024 bytes of printable ASCII",
            ));
        }
        let command = match command {
            RequestCommand::Http => "http",
            RequestCommand::Api => "api",
        };
        let agent = agent
            .map(|kind| format!("; {}", kind.label()))
            .unwrap_or_default();
        Ok(Self {
            user_agent: format!(
                "fabric-agent-cli/{} ({command}; {}; {}{agent})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH,
            ),
            session_id,
            skill: skill
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
        })
    }

    /// Add managed fields after custom-header and destination validation.
    ///
    /// The caller must not provide these fields. Correlation is reused across
    /// requests; skill is limited to the validated Fabric audience.
    pub fn apply(
        &self,
        headers: &mut BTreeMap<String, String>,
        audience: Audience,
    ) -> Result<(), CoreError> {
        headers.insert(USER_AGENT_HEADER.into(), self.user_agent.clone());
        if let Some(id) = &self.session_id {
            headers.insert(CLIENT_REQUEST_ID_HEADER.into(), id.to_string());
        }
        if audience == Audience::Fabric
            && let Some(skill) = &self.skill
        {
            headers.insert(FABRIC_SKILL_HEADER.into(), skill.clone());
        }
        let total: usize = headers
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum();
        if total > 64 * 1024 {
            return Err(CoreError::new(
                ErrorKind::InvalidInput,
                "HTTP headers including managed metadata exceed the 65536-byte total limit",
            ));
        }
        Ok(())
    }
}
