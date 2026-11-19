// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded OCV feedback input and wire preparation, without identity or I/O.

use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};
use serde_json::{Value, json};

/// Caller-authored content and explicit submission intent.
#[derive(Clone, Debug)]
pub struct FeedbackCommand {
    /// Short caller-authored subject (at most 256 UTF-8 bytes).
    pub title: String,
    /// Caller-authored feedback (at most 16 KiB UTF-8).
    pub message: String,
    /// Optional correlation UUID override; the executable generates one if absent.
    /// This is not a provider idempotency guarantee.
    pub feedback_id: Option<String>,
    /// Explicit authorization to submit, otherwise preparation only.
    pub send: bool,
    /// Explicit root output format; defaults to JSON without reading settings.
    pub output_format: Option<String>,
}

/// Closed service environment, independent of routing application ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    /// Production Petrol.
    Production,
    /// Integration/PPE Petrol, with a different token audience.
    Ppe,
}

impl Environment {
    /// Built-in OCV routing application shared by production and PPE.
    pub const fn app_id(self) -> u32 {
        2931
    }

    /// Stable configuration spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::Production => "prod",
            Self::Ppe => "ppe",
        }
    }

    /// Fixed credential destination; arbitrary URLs are never accepted.
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Production => "https://petrol.office.microsoft.com/v1/feedback/authenticated",
            Self::Ppe => "https://petrol-int.office.microsoft.com/v1/feedback/authenticated",
        }
    }
}

/// Validated nonsecret routing captured once by the caller.
#[derive(Clone, Debug)]
pub struct Configuration {
    /// Matched endpoint/token environment.
    pub environment: Environment,
    /// Optional owner-supplied OCV routing area.
    pub feature_area: Option<String>,
}

impl Configuration {
    /// Captures only documented environment variables, with no identity lookup.
    ///
    /// # Errors
    /// Rejects unreadable/invalid selectors and requires explicit environment for
    /// submission, so a missing PPE selector cannot silently send to production.
    pub fn capture(
        send: bool,
        mut read: impl FnMut(&str) -> Result<Option<String>, &'static str>,
    ) -> Result<Self, &'static str> {
        let environment = match read("FAB_RUST_FEEDBACK_ENVIRONMENT")?.as_deref() {
            Some("prod") => Environment::Production,
            Some("ppe") => Environment::Ppe,
            None if !send => Environment::Production,
            _ => {
                return Err(
                    "Set FAB_RUST_FEEDBACK_ENVIRONMENT explicitly to prod or ppe before sending.",
                );
            }
        };
        if read("FAB_RUST_FEEDBACK_APP_ID")?
            .is_some_and(|value| value != environment.app_id().to_string())
        {
            return Err(
                "FAB_RUST_FEEDBACK_APP_ID is no longer configurable. Unset it; FAB_RUST_FEEDBACK_ENVIRONMENT selects the built-in app ID.",
            );
        }
        let feature_area = read("FAB_RUST_FEEDBACK_FEATURE_AREA")?;
        if feature_area
            .as_ref()
            .is_some_and(|value| !single_line(value, 256))
        {
            return Err(
                "FAB_RUST_FEEDBACK_FEATURE_AREA must be nonempty, single-line and at most 256 UTF-8 bytes.",
            );
        }
        Ok(Self {
            environment,
            feature_area,
        })
    }
}

/// Builds the schema; parsing performs no environment or credential access.
pub fn command() -> Command {
    Command::new("feedback")
        .visible_alias("report")
        .about("Preview OCV feedback; --send submits caller-provided content")
        .arg(Arg::new("title").long("title").required(true))
        .arg(Arg::new("message").long("message").required(true))
        .arg(
            Arg::new("feedback-id")
                .long("feedback-id")
                .help("Optional correlation UUID override; a random UUID is generated automatically"),
        )
        .arg(Arg::new("send").long("send").action(ArgAction::SetTrue))
        .after_help("Preview is offline and never signs in. --send requires Windows with native-read and an explicitly selected broker account. Set FAB_RUST_FEEDBACK_ENVIRONMENT=ppe or prod to select its built-in endpoint, token scope and app ID (2931 for both). FAB_RUST_FEEDBACK_FEATURE_AREA is optional. Each invocation generates its own correlation UUID unless --feedback-id is supplied. Only supplied title/message and declared app metadata are sent: no automatic email, logs or attachments. HTTP acceptance does not prove downstream delivery. Never automatically repeat an unknown submission.")
}

pub(crate) fn from_matches(values: &ArgMatches) -> Result<FeedbackCommand, clap::Error> {
    let get = |key| values.get_one::<String>(key).cloned().unwrap_or_default();
    let command = FeedbackCommand {
        title: get("title"),
        message: get("message"),
        feedback_id: values
            .get_one::<String>("feedback-id")
            .map(|value| value.to_ascii_lowercase()),
        send: values.get_flag("send"),
        output_format: values.get_one::<String>("output_format").cloned(),
    };
    command
        .validate()
        .map_err(|message| clap::Error::raw(ErrorKind::InvalidValue, message))?;
    Ok(command)
}

impl FeedbackCommand {
    /// Validates public library callers as well as parsed CLI arguments.
    ///
    /// # Errors
    /// Returns only schema-owned text, never rejected caller values.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !single_line(&self.title, 256) {
            return Err(
                "Feedback title must be nonempty, single-line and at most 256 UTF-8 bytes.",
            );
        }
        if self.message.trim().is_empty()
            || self.message.len() > 16 * 1024
            || self
                .message
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
        {
            return Err(
                "Feedback message must be nonempty and at most 16384 UTF-8 bytes, without unsupported control characters.",
            );
        }
        if let Some(feedback_id) = &self.feedback_id {
            let id = feedback_id.as_bytes();
            if id.len() != 36
                || !id.iter().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        *byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
                || feedback_id == "00000000-0000-0000-0000-000000000000"
            {
                return Err("--feedback-id must be a nonzero hyphenated UUID.");
            }
        }
        if self
            .output_format
            .as_deref()
            .is_some_and(|v| !matches!(v, "json" | "text"))
        {
            return Err("Feedback output format must be json or text.");
        }
        Ok(())
    }

    /// Prepares the authenticated manifest using an explicit UTC clock.
    ///
    /// # Errors
    /// Rejects invalid command values before constructing a manifest.
    pub fn manifest(
        &self,
        config: &Configuration,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Value, &'static str> {
        self.validate()?;
        let feedback_id = self
            .feedback_id
            .as_ref()
            .ok_or("Feedback correlation ID must be prepared before constructing the manifest.")?;
        if config
            .feature_area
            .as_ref()
            .is_some_and(|value| !single_line(value, 256))
        {
            return Err("Invalid OCV routing configuration.");
        }
        let mut telemetry = json!({
            "audience": match config.environment {
                Environment::Production => "Production",
                Environment::Ppe => "PPE",
            }
        });
        if let Some(area) = &config.feature_area {
            telemetry["featureArea"] = json!(area);
        }
        let manifest = json!({
            "appId": config.environment.app_id(),
            "clientFeedbackId": feedback_id,
            "manifestType": "Sas",
            "source": "Client",
            "submissionType": "FormSubmit",
            "submitTime": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "sdkVersion": concat!("fab-rs/", env!("CARGO_PKG_VERSION")),
            "type": "Unclassified",
            "feedbackTitle": self.title,
            "comment": self.message,
            "application": {"appData": json!({
                "client": "fab-rs",
                "version": env!("CARGO_PKG_VERSION"),
                "environment": config.environment.name(),
            }).to_string()},
            "telemetry": telemetry,
            "complianceChecks": {"authenticationType": "AAD"},
            "userConsentTypes": {
                "emailConsentType": "Not Requested",
                "contentSamplesConsentType": "None",
            },
            "responseContent": {
                "displayLocale": "en-US",
                "elements": [{
                    "elementType": "Text",
                    "displayedQuestion": "Feedback",
                    "displayedQuestionInEnglish": "Feedback",
                    "userVerbatim": self.message,
                    "userVerbatimRequested": true,
                }],
            },
            "scope": "Private",
            "shouldPromoteToADO": false,
            "isLocalizationIssue": false,
        });
        if serde_json::to_vec(&manifest)
            .map_err(|_| "Feedback manifest encoding failed.")?
            .len()
            > 64 * 1024
        {
            return Err("Encoded feedback manifest exceeds 64 KiB.");
        }
        Ok(manifest)
    }
}

fn single_line(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

/// Sanitized submission outcome; correlation is client-owned, not a service receipt.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Delivery {
    /// `not_sent`, `accepted`, `rejected` or `unknown`, never downstream delivery.
    pub delivery: &'static str,
    /// Prepared correlation UUID, or absent when rejected before preparation.
    pub client_feedback_id: Option<String>,
    /// Fixed selected environment.
    pub environment: &'static str,
    /// Numeric OCV routing application.
    pub app_id: u32,
    /// HTTP status only when the response was read completely.
    pub http_status: Option<u16>,
    /// Schema-owned explanation with no provider bodies or credential details.
    pub message: &'static str,
}

impl Delivery {
    /// Constructs a sanitized host failure or interrupted/uncertain result.
    pub fn new(
        command: &FeedbackCommand,
        config: &Configuration,
        delivery: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            delivery,
            client_feedback_id: command.feedback_id.clone(),
            environment: config.environment.name(),
            app_id: config.environment.app_id(),
            http_status: None,
            message,
        }
    }

    /// Only completed 2xx ingestion responses are successful CLI submissions.
    pub fn accepted(&self) -> bool {
        self.delivery == "accepted"
    }
}

/// Submits one authenticated feedback occurrence with injected identity and HTTP.
///
/// The caller owns runtime, cancellation, clock and durable correlation capture.
/// Neither credential failures nor HTTP failures trigger replay or anonymous
/// fallback. The `send` flag is checked again for non-CLI callers.
#[cfg(feature = "native-host")]
pub async fn submit(
    command: &FeedbackCommand,
    config: &Configuration,
    now: chrono::DateTime<chrono::Utc>,
    credential: &dyn azure_core::credentials::TokenCredential,
    executor: &fabric_sdk_http::HttpExecutor,
) -> Delivery {
    use fabric_sdk_http::feedback::Environment as HttpEnvironment;
    let not_sent = |message| Delivery::new(command, config, "not_sent", message);
    if !command.send {
        return not_sent("Submission requires explicit --send.");
    }
    let body = match command
        .manifest(config, now)
        .ok()
        .and_then(|value| serde_json::to_vec(&value).ok())
        .filter(|value| value.len() <= 64 * 1024)
    {
        Some(body) => body,
        None => return not_sent("Invalid feedback manifest or encoded manifest exceeds 64 KiB."),
    };
    let environment = match config.environment {
        Environment::Production => HttpEnvironment::Production,
        Environment::Ppe => HttpEnvironment::Ppe,
    };
    let token = match credential.get_token(&[environment.scope()], None).await {
        Ok(token) => token,
        Err(_) => {
            return not_sent(
                "Feedback authentication failed. Verify the selected account and exact environment's preauthorization; no fallback or submission occurred.",
            );
        }
    };
    match executor.execute_feedback(environment, &token, &body).await {
        Ok(response) => {
            let (delivery, message) = match response.status {
                200..=299 => (
                    "accepted",
                    "OCV authenticated ingestion accepted the request. Downstream routing and display are not verified.",
                ),
                400..=407 | 409..=499 => (
                    "rejected",
                    "OCV rejected this request. Check the HTTP status, environment, routing and authorization. No automatic retry occurred.",
                ),
                _ => (
                    "unknown",
                    "Submission outcome is unconfirmed. Keep this correlation ID; reconcile before submitting again. No automatic retry occurred.",
                ),
            };
            let mut result = Delivery::new(command, config, delivery, message);
            result.http_status = Some(response.status);
            result
        }
        Err(fabric_sdk_http::ExecutionError::InvalidInput) => {
            not_sent("Invalid HTTP submission configuration; no request was sent.")
        }
        Err(_) => Delivery::new(
            command,
            config,
            "unknown",
            "The response was lost, exceeded its bound or timed out. Remote ingestion may have completed. Keep this correlation ID and reconcile before submitting again; no automatic retry occurred.",
        ),
    }
}
