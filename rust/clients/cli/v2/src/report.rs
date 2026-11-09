// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure feedback preparation and explicit manual handoff.
//!
//! This module never sends, saves, opens a browser, collects diagnostics, or
//! accesses configuration, environment variables, credentials or remote APIs.
//! The caller validates the `/.support/feedback` target before calling [`prepare`].
//! Payloads are caller-supplied UTF-8, preserved without trimming or enrichment.
//! Automatic delivery is not implemented; no provider receipt is produced.

use serde_json::json;

use crate::{CliError, OperationResult, Status};

/// Maximum title length in UTF-8 bytes, inclusive: 256 bytes.
pub const MAX_TITLE_BYTES: usize = 256;
/// Maximum message length in UTF-8 bytes, inclusive: 16,384 bytes.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

const GITHUB_PRIVACY: &str = "Repository visibility is unverified and repository-dependent. Public-repository issues are public; private-repository issues are visible to permitted repository users. Labels do not provide confidentiality. Do not put sensitive data in a public issue.";
const IDEAS_PRIVACY: &str = "Fabric Ideas is community feedback, not a confidential channel. Do not include sensitive data.";
const SECURITY_PRIVACY: &str = "Follow Microsoft's security reporting policy for suspected vulnerabilities. Do not report vulnerabilities through public GitHub issues or Fabric Ideas.";
const NO_DELIVERY: &str = "Nothing has been sent, saved or opened. Complete submission manually through the indicated route; review that provider's visibility and collection policy.";

/// Explicitly supported manual destinations, never implicit sending providers.
///
/// GitHub repository components are validated again by [`prepare`] so direct
/// construction cannot bypass [`Destination::parse`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Destination {
    /// An explicitly named GitHub.com repository; its visibility is not verified.
    GitHub {
        /// ASCII account name, at most 39 bytes, with single interior hyphens.
        owner: String,
        /// ASCII repository name, at most 100 bytes, using letters, digits, `._-`.
        repo: String,
    },
    /// Fabric's documented community Ideas submission route.
    FabricIdeas,
    /// Microsoft's security reporting policy and its actual reporting instructions.
    MicrosoftSecurity,
}

impl Destination {
    /// Parse only `github:owner/repo`, `ideas`, or `security`, without normalization.
    ///
    /// GitHub account names start and end with ASCII letters or digits and may
    /// contain single interior hyphens. Repository names use ASCII letters,
    /// digits, periods, underscores and hyphens, excluding `.` and `..`.
    /// URLs, query strings, percent-encoded components and additional paths are
    /// not accepted. No destination is inferred or substituted.
    ///
    /// # Errors
    /// Returns `InvalidInput` for an unsupported spelling or invalid component,
    /// without echoing the supplied value.
    pub fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "ideas" => Ok(Self::FabricIdeas),
            "security" => Ok(Self::MicrosoftSecurity),
            _ => {
                let (owner, repo) = value
                    .strip_prefix("github:")
                    .and_then(|value| value.split_once('/'))
                    .ok_or_else(invalid_destination)?;
                validate_repository(owner, repo)?;
                Ok(Self::GitHub {
                    owner: owner.to_owned(),
                    repo: repo.to_owned(),
                })
            }
        }
    }
}

/// Caller-owned input for a draft or manual handoff, never a sending request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportRequest {
    /// Exact manual destination chosen by the caller.
    pub destination: Destination,
    /// Nonblank title, at most [`MAX_TITLE_BYTES`], with no control characters.
    pub title: String,
    /// Nonblank body, at most [`MAX_MESSAGE_BYTES`]; newlines and tabs are allowed.
    ///
    /// Other control characters are rejected. The body is not collected from
    /// diagnostics and is never placed in a URL or sent to a provider.
    pub message: String,
    /// Explicitly classify the result as a manual handoff instead of a draft.
    ///
    /// This does not open the route, approve the provider or submit the payload.
    pub handoff: bool,
}

/// Prepare the exact supplied title/body and a canonical, unprefilled manual route.
///
/// Returns [`Status::Prepared`] by default or [`Status::Handoff`] when explicitly
/// requested. Both have an empty effect list and `delivery: "not_sent"`. No
/// approval or receipt is inferred. GitHub visibility is explicitly unverified;
/// Ideas is community feedback; security follows the repository's Microsoft
/// reporting policy at <https://aka.ms/SECURITY.md>.
///
/// UTF-8 byte limits are inclusive: 256 title bytes and 16,384 message bytes.
/// Nonblank content is preserved exactly, including surrounding whitespace.
///
/// # Errors
/// Returns `InvalidInput` for an invalid destination, blank or oversized payload,
/// or forbidden control characters. Errors do not echo the payload.
pub fn prepare(request: &ReportRequest) -> Result<OperationResult, CliError> {
    validate_payload(&request.title, &request.message)?;
    let (destination, manual_route, privacy) = match &request.destination {
        Destination::GitHub { owner, repo } => {
            validate_repository(owner, repo)?;
            (
                json!({
                    "kind": "github",
                    "owner": owner,
                    "repo": repo,
                    "visibility": "unverified_repository_dependent",
                }),
                format!("https://github.com/{owner}/{repo}/issues/new"),
                GITHUB_PRIVACY,
            )
        }
        Destination::FabricIdeas => (
            json!({"kind": "ideas", "visibility": "community_public"}),
            "https://community.fabric.microsoft.com/t5/Ideas/ct-p/PBI_Comm_Ideas".to_owned(),
            IDEAS_PRIVACY,
        ),
        Destination::MicrosoftSecurity => (
            json!({"kind": "security", "visibility": "governed_by_microsoft_security_policy"}),
            "https://aka.ms/SECURITY.md".to_owned(),
            SECURITY_PRIVACY,
        ),
    };
    Ok(OperationResult::new(
        if request.handoff {
            Status::Handoff
        } else {
            Status::Prepared
        },
        json!({
            "destination": destination,
            "payload": {"title": request.title, "body": request.message},
            "manual_route": manual_route,
            "delivery": "not_sent",
            "instructions": NO_DELIVERY,
            "warnings": [privacy],
            "limits": {
                "title_bytes": MAX_TITLE_BYTES,
                "message_bytes": MAX_MESSAGE_BYTES,
            },
        }),
    ))
}

fn invalid_destination() -> CliError {
    CliError::new(
        "InvalidInput",
        "Report destination must be github:owner/repo, ideas or security with valid components.",
    )
}

fn validate_repository(owner: &str, repo: &str) -> Result<(), CliError> {
    let owner_edge = |byte: Option<u8>| byte.is_some_and(|byte| byte.is_ascii_alphanumeric());
    if owner.len() > 39
        || !owner_edge(owner.as_bytes().first().copied())
        || !owner_edge(owner.as_bytes().last().copied())
        || owner.contains("--")
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || repo.is_empty()
        || repo.len() > 100
        || matches!(repo, "." | "..")
        || !repo
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Err(invalid_destination())
    } else {
        Ok(())
    }
}

fn validate_payload(title: &str, message: &str) -> Result<(), CliError> {
    if title.is_empty()
        || title.len() > MAX_TITLE_BYTES
        || title.chars().any(char::is_control)
        || title.trim().is_empty()
    {
        return Err(CliError::new(
            "InvalidInput",
            "Report title must be nonblank, at most 256 UTF-8 bytes, and contain no control characters.",
        ));
    }
    if message.is_empty()
        || message.len() > MAX_MESSAGE_BYTES
        || message
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        || message.trim().is_empty()
    {
        return Err(CliError::new(
            "InvalidInput",
            "Report message must be nonblank, at most 16384 UTF-8 bytes, and contain no control characters except newlines and tabs.",
        ));
    }
    Ok(())
}
