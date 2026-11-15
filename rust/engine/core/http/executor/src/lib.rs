// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Authenticated bounded execution. Only safe GETs may retry; mutations send once.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use chrono::Datelike;
use fabric_core::{
    ApiRequest, ApiResponse, Audience, Clock, CoreError, ErrorKind, HttpExecutor, HttpTransport,
    MAX_REQUEST_BYTES, OperationContext, ResponseHeadCapture, validate_headers,
    validate_operation_url, validate_response, validate_url,
};
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
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
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        let remaining = context.remaining()?;
        let delay = delay.min(remaining);
        if Instant::now().checked_add(delay).is_none() {
            return Err(CoreError::new(
                ErrorKind::Protocol,
                "Polling delay exceeds clock range",
            ));
        }
        bounded(context, tokio::time::sleep(delay)).await?;
        context.remaining()?;
        Ok(())
    }
}
/// Enforce deadline and cancellation by dropping a losing action future.
pub async fn bounded<T>(
    context: &OperationContext,
    action: impl Future<Output = T>,
) -> Result<T, CoreError> {
    let remaining = context.remaining()?;
    let value = race(context, remaining, action).await?;
    context.remaining()?;
    Ok(value)
}
/// Enforce the remaining injected-clock budget around a potentially uncooperative
/// action, including credentials and synthetic transports.
pub async fn bounded_with_clock<T>(
    clock: &dyn Clock,
    context: &OperationContext,
    action: impl Future<Output = T>,
) -> Result<T, CoreError> {
    let remaining = context.remaining_at(clock.now())?;
    let value = race(context, remaining, action).await?;
    context.remaining_at(clock.now())?;
    Ok(value)
}
async fn race<T>(
    context: &OperationContext,
    remaining: Duration,
    action: impl Future<Output = T>,
) -> Result<T, CoreError> {
    tokio::select! {
        biased;
        () = context.cancellation.cancelled() => Err(CoreError::new(ErrorKind::Cancelled, "Operation cancelled locally; backend work may continue")),
        () = async {
            if context.deadline.is_some() {
                tokio::time::sleep(remaining).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => Err(CoreError::new(ErrorKind::Timeout, "Operation deadline reached; backend work may continue")),
        value = action => Ok(value),
    }
}
/// Retry-After as a not-before delay; invalid syntax is an explicit safe diagnostic.
pub fn retry_after(response: &ApiResponse, now: SystemTime) -> Result<Option<Duration>, CoreError> {
    let Some(value) = response.header("retry-after") else {
        return Ok(None);
    };
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        // An arbitrarily large valid delta must not overflow into an early retry.
        return Ok(Some(
            value
                .parse::<u64>()
                .map(Duration::from_secs)
                .unwrap_or(Duration::MAX),
        ));
    }
    let then = http_date(value, now)
        .ok_or_else(|| CoreError::new(ErrorKind::Protocol, "Invalid Retry-After header"))?;
    Ok(Some(then.duration_since(now).unwrap_or_default()))
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
    credential: Arc<dyn TokenCredential>,
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}
impl HttpClient {
    /// Compose reusable execution without ambient discovery or global runtime setup.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        transport: Arc<dyn HttpTransport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            credential,
            transport,
            clock,
        }
    }
    async fn acquire(
        &self,
        audience: Audience,
        context: &OperationContext,
    ) -> Result<AccessToken, CoreError> {
        context.remaining_at(self.clock.now())?;
        let scopes = [audience.scope()];
        bounded_with_clock(
            self.clock.as_ref(),
            context,
            self.credential.get_token(&scopes, Some(TokenRequestOptions::default())),
        )
        .await?
        .map_err(|_| CoreError::new(ErrorKind::Unauthorized,
            "Credential acquisition failed; run fab-rs auth login to validate the selected session"))
    }
}
impl HttpClient {
    async fn execute_inner(
        &self,
        request: ApiRequest,
        audience: Audience,
        context: &OperationContext,
        retry_get: bool,
    ) -> Result<ApiResponse, CoreError> {
        if retry_get {
            if request.method != "GET" || !request.body.is_empty() || !request.headers.is_empty() {
                return Err(CoreError::new(
                    ErrorKind::InvalidInput,
                    "Polling requests must be bodyless GETs without custom headers",
                ));
            }
            validate_operation_url(audience, request.url.as_str())?;
        } else {
            validate_url(audience, &request.url)?;
        }
        validate_headers(&request.headers)?;
        if request.body.len() > MAX_REQUEST_BYTES
            || !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&request.method.as_str())
        {
            return Err(CoreError::new(
                ErrorKind::InvalidInput,
                "Request body exceeds allocation bound",
            ));
        }
        let safe = retry_get && request.method == "GET";
        let mut delay = Duration::from_secs(1);
        loop {
            context.remaining_at(self.clock.now())?;
            let token = self.acquire(audience, context).await?;
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
                return Err(CoreError::new(
                    ErrorKind::Unauthorized,
                    "Credential returned an invalid bearer token",
                ));
            }
            let mut authenticated = request.clone();
            authenticated
                .headers
                .insert("authorization".into(), format!("Bearer {bearer}"));
            context.remaining_at(self.clock.now())?;
            let head = ResponseHeadCapture::new(audience);
            let result = bounded_with_clock(
                self.clock.as_ref(),
                context,
                self.transport
                    .send_with_observer(authenticated, context, &head),
            )
            .await
            .map_err(|error| head.attach(error))?
            .map_err(|error| head.attach(error));
            context
                .remaining_at(self.clock.now())
                .map_err(|error| head.attach(error))?;
            let retry = match &result {
                Ok(response) => {
                    validate_response(response).map_err(|error| head.attach(error))?;
                    if (300..400).contains(&response.status) {
                        return Err(CoreError::new(
                            ErrorKind::Protocol,
                            "HTTP redirects are not followed",
                        ));
                    }
                    safe && matches!(response.status, 408 | 429 | 500 | 502 | 503 | 504)
                }
                Err(error) => safe && error.kind == ErrorKind::Transport,
            };
            if !retry {
                return result;
            }
            let server_delay =
                result
                    .as_ref()
                    .ok()
                    .and_then(|r| match retry_after(r, self.clock.wall_now()) {
                        Ok(delay) => delay,
                        Err(_) => {
                            self.clock
                                .diagnostic("Invalid Retry-After header; using client backoff");
                            None
                        }
                    });
            sleep_with_budget(
                self.clock.as_ref(),
                delay.max(server_delay.unwrap_or_default()),
                context,
            )
            .await?;
            context.remaining_at(self.clock.now())?;
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
