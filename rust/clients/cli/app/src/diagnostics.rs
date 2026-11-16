// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Console formatting accepts only the engine's bounded diagnostic vocabulary.

use fabric_core::{
    CoreError, ErrorKind,
    diagnostics::{DiagnosticEvent, DiagnosticPhase, Metadata, SafeHeaders},
};
use std::{fmt::Display, io::Write, time::Duration};

const MAX_EVENT_BYTES: usize = 2048;
const MAX_EVENT_LINES: usize = 8;

pub(crate) fn error() -> CoreError {
    CoreError::new(ErrorKind::Diagnostic, "Cannot write verbose diagnostics")
}

pub(crate) fn category(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::InvalidInput => "invalid input",
        ErrorKind::Unauthorized => "unauthorized",
        ErrorKind::Transport => "transport failure",
        ErrorKind::Protocol => "protocol failure",
        ErrorKind::Timeout => "timeout",
        ErrorKind::Cancelled => "cancelled",
        ErrorKind::Service => "service failure",
        ErrorKind::Diagnostic => "diagnostic output failure",
    }
}

fn phase_text(phase: DiagnosticPhase) -> String {
    match phase {
        DiagnosticPhase::Failed(kind) => format!("failed ({})", category(kind)),
        _ => phase.to_string(),
    }
}

fn seconds(milliseconds: u64) -> String {
    if milliseconds.is_multiple_of(1000) {
        format!("{} s", milliseconds / 1000)
    } else {
        format!("{}.{:03} s", milliseconds / 1000, milliseconds % 1000)
    }
}

fn header<T: Display>(block: &mut String, name: &str, value: Metadata<T>) {
    if !matches!(value, Metadata::Absent) {
        block.push_str(&format!("    {name}: {value}\n"));
    }
}

fn headers(block: &mut String, values: SafeHeaders) {
    header(block, "Content-Type", values.content_type());
    header(block, "Content-Length", values.content_length());
    header(block, "Content-Encoding", values.content_encoding());
    header(block, "Retry-After", values.retry_after());
    header(block, "Request ID", values.request_id());
    header(block, "x-ms-request-id", values.x_ms_request_id());
    header(block, "Location", values.location());
}

pub(crate) fn write(
    event: &DiagnosticEvent,
    elapsed: Duration,
    color: bool,
    stderr: &mut dyn Write,
) -> Result<(), CoreError> {
    let message = match event {
        DiagnosticEvent::Stage {
            stage,
            phase,
            duration,
        } => {
            format!("{stage}: {} ({duration})", phase_text(*phase))
        }
        DiagnosticEvent::Credential { phase, duration } => {
            format!("Credentials: {} ({duration})", phase_text(*phase))
        }
        DiagnosticEvent::HttpSend {
            id,
            attempt,
            method,
            url,
            ..
        } => {
            format!("HTTP #{id} sending {method} {url} (attempt {attempt})")
        }
        DiagnosticEvent::HttpResponse {
            id,
            status,
            duration,
            ..
        } => {
            let status = status.map_or_else(|| "<omitted>".into(), |value| value.to_string());
            format!("HTTP #{id} received {status} ({duration})")
        }
        DiagnosticEvent::HttpFailure {
            id,
            kind,
            status,
            duration,
        } => {
            let response = status.map_or_else(String::new, |value| format!("; received {value}"));
            format!(
                "HTTP #{id} failed ({}{response}, {duration})",
                category(*kind)
            )
        }
        DiagnosticEvent::Retry {
            id,
            attempt,
            delay,
            reason,
            source,
        } => {
            format!(
                "HTTP #{id} retry: attempt {attempt} in {} ({reason}; {source})",
                seconds(delay.millis()),
            )
        }
        DiagnosticEvent::Polling { strategy } => format!("Polling: {strategy}"),
        DiagnosticEvent::PollWait {
            delay,
            purpose,
            source,
        } => {
            format!(
                "Polling: {purpose} in {} ({source})",
                seconds(delay.millis())
            )
        }
        DiagnosticEvent::PollResult { action } => format!("Polling: {action}"),
        DiagnosticEvent::PollCompleted { outcome } => format!("Polling: backend {outcome}"),
        DiagnosticEvent::PollFailed { kind } => format!("Polling: stopped ({})", category(*kind)),
    };
    let label = format!(
        "[debug +{}.{:03}s]",
        elapsed.as_secs(),
        elapsed.subsec_millis()
    );
    let mut block = if color {
        format!("\x1b[2;36m{label}\x1b[0m {message}\n")
    } else {
        format!("{label} {message}\n")
    };
    match event {
        DiagnosticEvent::HttpSend {
            headers: values, ..
        }
        | DiagnosticEvent::HttpResponse {
            headers: values, ..
        } => headers(&mut block, *values),
        _ => {}
    }
    if block.len() > MAX_EVENT_BYTES || block.lines().count() > MAX_EVENT_LINES {
        return Err(CoreError::new(
            ErrorKind::Diagnostic,
            "Verbose diagnostic exceeds the console event bound",
        ));
    }
    stderr
        .write_all(block.as_bytes())
        .and_then(|()| stderr.flush())
        .map_err(|_| error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_core::diagnostics::{
        ContentEncoding, ContentType, DiagnosticDuration, DiagnosticMethod, DiagnosticOrigin,
        DiagnosticRoute, RequestId, SafeUrl,
    };

    #[test]
    fn maximum_metadata_is_bounded_and_only_renderer_adds_color() {
        let event = DiagnosticEvent::HttpSend {
            id: u64::MAX,
            attempt: u32::MAX,
            method: DiagnosticMethod::Delete,
            url: SafeUrl::new(DiagnosticOrigin::Storage, DiagnosticRoute::JobInstance),
            headers: SafeHeaders::new(
                Metadata::Value(ContentType::Multipart),
                Metadata::Value(u64::MAX),
                Metadata::Value(ContentEncoding::Deflate),
                Metadata::Value(DiagnosticDuration::from_duration(Duration::MAX)),
                Metadata::Value(RequestId::parse("01234567-89ab-cdef-0123-456789abcdef").unwrap()),
                Metadata::Omitted,
                Metadata::Value(SafeUrl::new(
                    DiagnosticOrigin::Storage,
                    DiagnosticRoute::JobInstance,
                )),
            ),
        };
        let mut plain = Vec::new();
        write(&event, Duration::MAX, false, &mut plain).unwrap();
        let text = String::from_utf8(plain).unwrap();
        assert_eq!(text.lines().count(), 8);
        assert!(text.len() <= MAX_EVENT_BYTES);
        assert!(!text.contains('\x1b'));
        assert!(text.contains("    x-ms-request-id: <omitted>\n"));
        let mut colored = Vec::new();
        write(&event, Duration::MAX, true, &mut colored).unwrap();
        let colored = String::from_utf8(colored).unwrap();
        assert_eq!(
            colored.replace("\x1b[2;36m", "").replace("\x1b[0m", ""),
            text
        );
        assert_eq!(colored.matches('\x1b').count(), 2);
    }
}
