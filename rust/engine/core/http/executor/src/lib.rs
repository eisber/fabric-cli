// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Authenticated bounded execution. Only safe GETs may retry; mutations send once.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use chrono::Datelike;
pub use fabric_core::bounded_with_clock;
use fabric_core::diagnostics::{
    DelaySource, DiagnosticDuration, DiagnosticEvent, DiagnosticMethod, DiagnosticObserver,
    DiagnosticOutcome, DiagnosticPhase, HttpStatus, RetryReason,
};
use fabric_core::{
    ApiRequest, ApiResponse, Audience, Clock, CoreError, Endpoint, ErrorKind, HttpExecutor,
    HttpTransport, MAX_REQUEST_BYTES, OperationContext, ResponseHeadCapture,
    resolve_operation_url_with_endpoint, validate_headers, validate_response,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
mod diagnostics;
use diagnostics::{
    safe_request_headers, safe_response_headers_with_endpoint, safe_url_with_endpoint,
};

/// Production monotonic time and cancellable Tokio sleep; host owns the runtime.
pub struct SystemClock;
#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::now()
    }
    async fn wait_until(
        &self,
        deadline: Instant,
        context: &OperationContext,
    ) -> Result<(), CoreError> {
        let remaining = deadline
            .checked_duration_since(self.now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                CoreError::new(
                    ErrorKind::Timeout,
                    "Operation deadline reached; backend work may continue",
                )
            })?;
        tokio::select! {
            biased;
            () = context.cancellation.cancelled() => Err(CoreError::new(
                ErrorKind::Cancelled,
                "Operation cancelled locally; backend work may continue",
            )),
            () = tokio::time::sleep(remaining) => {
                if context.remaining_at(self.now()).is_err() {
                    Err(CoreError::new(
                        ErrorKind::Timeout,
                        "Operation deadline reached; backend work may continue",
                    ))
                } else {
                    Err(CoreError::new(
                        ErrorKind::Protocol,
                        "Production clock returned before the operation deadline",
                    ))
                }
            }
        }
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        let remaining = context.remaining_at(self.now())?;
        let delay = delay.min(remaining);
        if self.now().checked_add(delay).is_none() {
            return Err(CoreError::new(
                ErrorKind::Protocol,
                "Polling delay exceeds clock range",
            ));
        }
        tokio::select! {
            biased;
            () = context.cancellation.cancelled() => Err(CoreError::new(
                ErrorKind::Cancelled,
                "Operation cancelled locally; backend work may continue",
            )),
            () = tokio::time::sleep(delay) => context.remaining_at(self.now()).map(|_| ()),
        }
    }
}
/// Retry-After as a not-before delay; invalid syntax is an explicit safe diagnostic.
pub fn retry_after(response: &ApiResponse, now: SystemTime) -> Result<Option<Duration>, CoreError> {
    let Some(value) = response.header("retry-after") else {
        return Ok(None);
    };
    retry_after_value(value, now).map(Some)
}
fn retry_after_value(value: &str, now: SystemTime) -> Result<Duration, CoreError> {
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        // An arbitrarily large valid delta must not overflow into an early retry.
        return Ok(value
            .parse::<u64>()
            .map(Duration::from_secs)
            .unwrap_or(Duration::MAX));
    }
    let then = http_date(value, now)
        .ok_or_else(|| {
            CoreError::new(
                ErrorKind::Protocol,
                "Service returned an invalid Retry-After header; expected delay seconds or an HTTP date",
            )
        })?;
    Ok(then.duration_since(now).unwrap_or_default())
}

fn http_date(value: &str, now: SystemTime) -> Option<SystemTime> {
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(value) {
        return Some(date.with_timezone(&chrono::Utc).into());
    }
    // HTTP recipients also accept obsolete RFC850 and asctime dates.
    if let Some((weekday, tail)) = value.split_once(", ") {
        let date = chrono::NaiveDateTime::parse_from_str(tail, "%d-%b-%y %H:%M:%S GMT").ok()?;
        let current_year = chrono::DateTime::<chrono::Utc>::from(now).year();
        let mut year = current_year.div_euclid(100) * 100 + date.year().rem_euclid(100);
        if year > current_year + 50 {
            year -= 100;
        }
        let date = date.with_year(year)?;
        return (date.format("%A").to_string() == weekday).then(|| date.and_utc().into());
    }
    chrono::NaiveDateTime::parse_from_str(value, "%a %b %e %H:%M:%S %Y")
        .ok()
        .map(|date| date.and_utc().into())
}

/// Sleep under the same operation budget, enforcing server not-before time even
/// with an injected sleeper. No new action may begin at or after the deadline.
pub async fn sleep_with_budget(
    clock: &dyn Clock,
    delay: Duration,
    context: &OperationContext,
) -> Result<(), CoreError> {
    let before = clock.now();
    let remaining = context.remaining_at(before)?;
    if before.checked_add(delay.min(remaining)).is_none() {
        return Err(CoreError::new(
            ErrorKind::Protocol,
            "Polling delay exceeds clock range",
        ));
    }
    bounded_with_clock(clock, context, clock.sleep(delay.min(remaining), context)).await??;
    let after = clock.now();
    context.remaining_at(after)?;
    if delay >= remaining {
        return Err(CoreError::new(
            ErrorKind::Timeout,
            "Operation deadline reached before server not-before time",
        ));
    }
    if after.checked_duration_since(before).unwrap_or_default() < delay {
        return Err(CoreError::new(
            ErrorKind::Protocol,
            "Injected sleeper returned before the not-before time",
        ));
    }
    Ok(())
}
/// Explicitly composed HTTP executor with injected credentials, transport, and time.
pub struct HttpClient {
    metadata: fabric_core::RequestMetadata,
    credential: Arc<dyn TokenCredential>,
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
    diagnostics: Option<Arc<dyn DiagnosticObserver>>,
    endpoint: Option<Endpoint>,
    next_request: AtomicU64,
}
impl HttpClient {
    /// Compose reusable execution without ambient discovery or global runtime setup.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        transport: Arc<dyn HttpTransport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            metadata: fabric_core::RequestMetadata::default(),
            credential,
            transport,
            clock,
            diagnostics: None,
            endpoint: None,
            next_request: AtomicU64::new(1),
        }
    }
    /// Enable explicit caller-owned diagnostics. The default constructor is silent.
    pub fn with_diagnostics(mut self, observer: Arc<dyn DiagnosticObserver>) -> Self {
        self.diagnostics = Some(observer);
        self
    }
    /// Use validated invocation metadata for all initial, polling, and result requests.
    pub fn with_request_metadata(mut self, metadata: fabric_core::RequestMetadata) -> Self {
        self.metadata = metadata;
        self
    }
    /// Bind execution to an explicitly validated endpoint.
    pub fn with_endpoint(mut self, endpoint: Endpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }
    async fn emit(
        &self,
        event: DiagnosticEvent,
        context: &OperationContext,
        outcome: &ExecutionOutcome,
    ) -> Result<(), CoreError> {
        let Some(observer) = &self.diagnostics else {
            return Ok(());
        };
        match bounded_with_clock(self.clock.as_ref(), context, observer.observe(event)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(outcome.diagnostic_error()),
            Err(error) => Err(outcome.attach(error)),
        }
    }
    fn elapsed(&self, start: Instant) -> DiagnosticDuration {
        DiagnosticDuration::from_duration(
            self.clock
                .now()
                .checked_duration_since(start)
                .unwrap_or_default(),
        )
    }
    async fn acquire(
        &self,
        audience: Audience,
        context: &OperationContext,
        outcome: &ExecutionOutcome,
    ) -> Result<AccessToken, CoreError> {
        context.remaining_at(self.clock.now())?;
        self.emit(
            DiagnosticEvent::Credential {
                phase: DiagnosticPhase::Started,
                duration: DiagnosticDuration::from_duration(Duration::ZERO),
            },
            context,
            outcome,
        )
        .await?;
        let start = self.clock.now();
        let scopes = [audience.scope()];
        let result = bounded_with_clock(
            self.clock.as_ref(),
            context,
            self.credential.get_token(&scopes, Some(TokenRequestOptions::default())),
        )
        .await
        .and_then(|result| result.map_err(|_| CoreError::new(ErrorKind::Unauthorized,
            "Credential acquisition failed; run fab-rs auth login to validate the selected session")))
        .and_then(|token| {
            let bearer = token.token.secret();
            let now = match self.clock.wall_now().duration_since(SystemTime::UNIX_EPOCH) {
                Ok(duration) => duration.as_nanos() as i128,
                Err(error) => -(error.duration().as_nanos() as i128),
            };
            if bearer.bytes().any(|b| b <= 32 || b >= 127)
                || bearer.is_empty()
                || bearer.len() > 16 * 1024
                || token.expires_on.unix_timestamp_nanos() <= now
            {
                Err(CoreError::new(ErrorKind::Unauthorized, "Credential returned an invalid bearer token"))
            } else {
                Ok(token)
            }
        });
        if result
            .as_ref()
            .is_err_and(|error| matches!(error.kind, ErrorKind::Cancelled | ErrorKind::Timeout))
        {
            return result;
        }
        self.emit(
            DiagnosticEvent::Credential {
                phase: match &result {
                    Ok(_) => DiagnosticPhase::Completed,
                    Err(error) => DiagnosticPhase::Failed(error.kind),
                },
                duration: self.elapsed(start),
            },
            context,
            outcome,
        )
        .await?;
        result
    }
}

#[derive(Default)]
struct ExecutionOutcome {
    sent: bool,
    status: Option<u16>,
    recovery: Option<fabric_core::RecoveryInfo>,
}
impl ExecutionOutcome {
    fn attach(&self, mut error: CoreError) -> CoreError {
        if error.recovery.is_none() {
            error.recovery = self.recovery.clone();
        }
        error
    }
    fn diagnostic_error(&self) -> CoreError {
        let outcome = if let Some(status) = self.status.and_then(HttpStatus::new) {
            DiagnosticOutcome::Received(status)
        } else if self.sent {
            DiagnosticOutcome::Uncertain
        } else {
            DiagnosticOutcome::BeforeSend
        };
        self.attach(CoreError::diagnostic_failure(outcome))
    }
    fn capture(&mut self, head: &ResponseHeadCapture, result: &Result<ApiResponse, CoreError>) {
        self.status = head
            .status()
            .or_else(|| result.as_ref().ok().map(|response| response.status));
        if let Some(recovery) = head.recovery().or_else(|| {
            result
                .as_ref()
                .err()
                .and_then(|error| error.recovery.clone())
        }) {
            self.recovery = Some(recovery);
        }
    }
}
impl HttpClient {
    async fn execute_inner(
        &self,
        mut request: ApiRequest,
        audience: Audience,
        context: &OperationContext,
        retry_get: bool,
    ) -> Result<ApiResponse, CoreError> {
        let endpoint = match &self.endpoint {
            Some(endpoint) if endpoint.audience() == audience => endpoint.clone(),
            Some(_) => {
                return Err(CoreError::new(
                    ErrorKind::InvalidInput,
                    "Configured endpoint does not match the request audience",
                ));
            }
            None => Endpoint::public(audience),
        };
        if retry_get {
            if request.method != "GET" || !request.body.is_empty() || !request.headers.is_empty() {
                return Err(CoreError::new(
                    ErrorKind::InvalidInput,
                    "Polling requests must be bodyless GETs without custom headers",
                ));
            }
            resolve_operation_url_with_endpoint(
                &endpoint,
                request.url.as_str(),
                endpoint.base_url(),
            )?;
        } else {
            endpoint.validate_url(&request.url)?;
        }
        validate_headers(&request.headers)?;
        if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&request.method.as_str()) {
            return Err(CoreError::new(
                ErrorKind::InvalidInput,
                "Unsupported HTTP method; use GET, POST, PUT, PATCH, or DELETE",
            ));
        }
        if request.body.len() > MAX_REQUEST_BYTES {
            return Err(CoreError::new(
                ErrorKind::InvalidInput,
                format!(
                    "Request body is {} bytes, exceeding the {MAX_REQUEST_BYTES}-byte limit",
                    request.body.len()
                ),
            ));
        }
        self.metadata.apply(&mut request.headers, audience)?;
        let safe = retry_get && request.method == "GET";
        let mut delay = Duration::from_secs(1);
        let mut attempt = 0u32;
        let mut outcome = ExecutionOutcome::default();
        loop {
            context
                .remaining_at(self.clock.now())
                .map_err(|error| outcome.attach(error))?;
            attempt = attempt.saturating_add(1);
            let token = self
                .acquire(audience, context, &outcome)
                .await
                .map_err(|error| outcome.attach(error))?;
            let bearer = token.token.secret();
            let mut authenticated = request.clone();
            authenticated
                .headers
                .insert("authorization".into(), format!("Bearer {bearer}"));
            let id = self
                .next_request
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                    Some(id.saturating_add(1))
                })
                .expect("request counter always advances or saturates");
            if self.diagnostics.is_some() {
                let method = match request.method.as_str() {
                    "GET" => DiagnosticMethod::Get,
                    "POST" => DiagnosticMethod::Post,
                    "PUT" => DiagnosticMethod::Put,
                    "PATCH" => DiagnosticMethod::Patch,
                    "DELETE" => DiagnosticMethod::Delete,
                    _ => unreachable!("method validated before credentials"),
                };
                // Validation above is authoritative; sanitization cannot expand
                // the set of destinations that execution is allowed to follow.
                let url = safe_url_with_endpoint(&endpoint, request.url.as_str())
                    .expect("validated request URL has safe metadata");
                self.emit(
                    DiagnosticEvent::HttpSend {
                        id,
                        attempt,
                        method,
                        url,
                        headers: safe_request_headers(&request.headers),
                    },
                    context,
                    &outcome,
                )
                .await?;
            }
            context
                .remaining_at(self.clock.now())
                .map_err(|error| outcome.attach(error))?;
            let head = ResponseHeadCapture::with_endpoint(endpoint.clone());
            let start = self.clock.now();
            outcome.sent = true;
            outcome.status = None;
            let mut result = bounded_with_clock(
                self.clock.as_ref(),
                context,
                self.transport.send_with_observer(
                    authenticated,
                    context,
                    self.clock.as_ref(),
                    &head,
                ),
            )
            .await
            .and_then(|result| result)
            .map_err(|error| head.attach(error));
            if let Ok(response) = &result
                && head.status().is_none()
                && let Err(error) = head.record(response.status, &response.headers)
            {
                result = Err(head.attach(error));
            }
            // Evidence must outlive a failing observer. In particular a 202
            // cannot be turned into a pre-send failure or lose its handle.
            outcome.capture(&head, &result);
            if result
                .as_ref()
                .is_err_and(|error| matches!(error.kind, ErrorKind::Cancelled | ErrorKind::Timeout))
            {
                return result.map_err(|error| outcome.attach(error));
            }
            context
                .remaining_at(self.clock.now())
                .map_err(|error| outcome.attach(head.attach(error)))?;
            if self.diagnostics.is_some() {
                let event = match &result {
                    Ok(response) => DiagnosticEvent::HttpResponse {
                        id,
                        status: HttpStatus::new(response.status),
                        duration: self.elapsed(start),
                        headers: safe_response_headers_with_endpoint(
                            &endpoint,
                            &response.headers,
                            request.url.as_str(),
                            self.clock.wall_now(),
                        ),
                    },
                    Err(error) => DiagnosticEvent::HttpFailure {
                        id,
                        status: outcome.status.and_then(HttpStatus::new),
                        kind: error.kind,
                        duration: self.elapsed(start),
                    },
                };
                self.emit(event, context, &outcome).await?;
            }
            let retry = match &result {
                Ok(response) => {
                    validate_response(response)
                        .map_err(|error| outcome.attach(head.attach(error)))?;
                    if (300..400).contains(&response.status) {
                        return Err(outcome.attach(CoreError::new(
                            ErrorKind::Protocol,
                            format!(
                                "Service responded with HTTP {} redirect; redirects are not followed",
                                response.status
                            ),
                        )));
                    }
                    safe && matches!(response.status, 408 | 429 | 500 | 502 | 503 | 504)
                }
                Err(error) => safe && error.kind == ErrorKind::Transport,
            };
            if !retry {
                return result.map_err(|error| outcome.attach(error));
            }
            let mut source = DelaySource::Client;
            let server_delay =
                result
                    .as_ref()
                    .ok()
                    .and_then(|r| match retry_after(r, self.clock.wall_now()) {
                        Ok(delay) => delay,
                        Err(_) => {
                            source = DelaySource::InvalidServerHint;
                            self.clock
                                .diagnostic("Invalid Retry-After header; using client backoff");
                            None
                        }
                    });
            let wait = delay.max(server_delay.unwrap_or_default());
            if server_delay.is_some_and(|server| server >= delay) {
                source = DelaySource::Server;
            }
            self.emit(
                DiagnosticEvent::Retry {
                    id,
                    attempt: attempt.saturating_add(1),
                    delay: DiagnosticDuration::from_duration(wait),
                    reason: if result.is_ok() {
                        RetryReason::HttpStatus
                    } else {
                        RetryReason::Transport
                    },
                    source,
                },
                context,
                &outcome,
            )
            .await?;
            sleep_with_budget(self.clock.as_ref(), wait, context)
                .await
                .map_err(|error| outcome.attach(error))?;
            context
                .remaining_at(self.clock.now())
                .map_err(|error| outcome.attach(error))?;
            delay = delay.saturating_mul(2);
        }
    }
}
#[async_trait]
impl HttpExecutor for HttpClient {
    async fn execute(
        &self,
        request: ApiRequest,
        audience: Audience,
        context: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        self.execute_inner(request, audience, context, false).await
    }
    async fn execute_poll(
        &self,
        request: ApiRequest,
        audience: Audience,
        context: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        self.execute_inner(request, audience, context, true).await
    }
}
