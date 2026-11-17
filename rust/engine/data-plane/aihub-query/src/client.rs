// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    DATA_AGENT_VARIANT, Limits, NativeDeadline, QueryError, QueryErrorKind, QueryMode,
    QueryOutcome, QueryRequest, QueryStage, SANDBOX_VARIANT, TEMPLATE_VARIANT,
    TYPED_RESULTS_VARIANT, decode, protocol,
};
use azure_core::{
    credentials::{AccessToken, TokenCredential},
    http::{HttpClient, Method, Request, Url},
};
use fabric_sdk_http::ExecutionTimer;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt,
    future::{Future, poll_fn},
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

/// Explicit endpoint and auth binding; it is not evidence that a service accepts it.
///
/// The caller must qualify the exact endpoint, token scope, tenant and account.
/// The credential is opaque: this SDK neither inspects tokens nor discovers or
/// switches identities. Production S2sObo is not assumed to accept user tokens.
#[derive(Clone)]
pub struct AuthBinding {
    endpoint: Url,
    scope: String,
    tenant: String,
    account: String,
}

impl AuthBinding {
    /// Validate a caller-injected exact HTTPS endpoint and explicit auth context.
    ///
    /// No default hostname, audience, account, tenant or auth provider is selected.
    pub fn new(
        endpoint: Url,
        scope: String,
        tenant: String,
        account: String,
    ) -> Result<Self, QueryError> {
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.as_str().len() > 4096
            || endpoint.path().contains(['%', '\\'])
            || [&scope, &tenant, &account]
                .into_iter()
                .any(|s| s.trim().is_empty() || s.len() > 2048 || s.chars().any(char::is_control))
            || !scope.ends_with("/.default")
        {
            return Err(QueryError::before(QueryErrorKind::InvalidInput));
        }
        Ok(Self {
            endpoint,
            scope,
            tenant,
            account,
        })
    }

    /// Exact caller-selected endpoint; no route or ring rewriting occurs.
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
    /// Exact requested scope, not inferred from the endpoint.
    pub fn scope(&self) -> &str {
        &self.scope
    }
    /// Caller-attested tenant binding; the SDK cannot inspect an opaque token.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    /// Caller-attested account binding; no alternate account is attempted.
    pub fn account(&self) -> &str {
        &self.account
    }
}

impl fmt::Debug for AuthBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthBinding(<explicit, redacted>)")
    }
}

/// Independently callable AIHub SDK, with no CLI or identity implementation.
///
/// Supply the existing Azure HTTP transport configured for streaming, no
/// redirects and no automatic retries. A complete call initializes a fresh MCP
/// session, acknowledges initialization, discovers the tool and calls it once.
/// Only negotiated session IDs are propagated; no Tasks/reconnect/polling API
/// is invented. No embedded resource URI is ever fetched.
#[derive(Clone)]
pub struct AiHubQueryClient {
    binding: AuthBinding,
    credential: Arc<dyn TokenCredential>,
    http: Arc<dyn HttpClient>,
    timer: Arc<dyn ExecutionTimer>,
    limits: Limits,
}

impl fmt::Debug for AiHubQueryClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AiHubQueryClient(<injected, redacted>)")
    }
}

impl AiHubQueryClient {
    /// Compose validated caller dependencies without acquiring credentials or I/O.
    pub fn new(
        binding: AuthBinding,
        credential: Arc<dyn TokenCredential>,
        http: Arc<dyn HttpClient>,
        timer: Arc<dyn ExecutionTimer>,
        limits: Limits,
    ) -> Result<Self, QueryError> {
        Ok(Self {
            binding,
            credential,
            http,
            timer,
            limits: limits.validate()?,
        })
    }

    /// Execute once under one absolute budget, including credentials and decoding.
    ///
    /// Cancellation wins simultaneous ready results. Drop cancels local work only.
    /// Errors retain whether query HTTP dispatch began, never permission to replay.
    /// A truncated observation is explicitly [`QueryOutcome::Incomplete`].
    pub async fn execute_query(
        &self,
        request: &QueryRequest,
        deadline: &NativeDeadline,
        canceled: impl Future<Output = ()> + Send,
    ) -> Result<QueryOutcome, QueryError> {
        let dispatched = AtomicBool::new(false);
        let remaining = deadline.remaining().unwrap_or(Duration::ZERO);
        let mut timeout = self.timer.sleep(remaining);
        let canceled = Cancellation {
            future: Mutex::new(Box::pin(canceled)),
            latched: AtomicBool::new(false),
        };
        let mut operation = pin!(self.run(request, deadline, &dispatched, &canceled));
        let result = poll_fn(|cx| {
            if canceled.poll(cx) {
                return Poll::Ready(Err(QueryErrorKind::Canceled));
            }
            if timeout.as_mut().poll(cx).is_ready() || deadline.remaining().is_err() {
                return Poll::Ready(Err(QueryErrorKind::Deadline));
            }
            let result = operation.as_mut().poll(cx);
            if canceled.poll(cx) {
                return Poll::Ready(Err(QueryErrorKind::Canceled));
            }
            if timeout.as_mut().poll(cx).is_ready() || deadline.remaining().is_err() {
                return Poll::Ready(Err(QueryErrorKind::Deadline));
            }
            result
        })
        .await;
        result.map_err(|kind| QueryError {
            kind,
            query_dispatched: dispatched.load(Ordering::SeqCst),
        })
    }

    async fn run(
        &self,
        request: &QueryRequest,
        deadline: &NativeDeadline,
        dispatched: &AtomicBool,
        canceled: &Cancellation<'_>,
    ) -> Result<QueryOutcome, QueryErrorKind> {
        canceled.check().await?;
        let token = self
            .credential
            .get_token(&[&self.binding.scope], None)
            .await
            .map_err(|_| QueryErrorKind::Authentication)?;
        let secret = token.token.secret();
        if secret.is_empty() || secret.len() > 32 * 1024 || secret.chars().any(char::is_control) {
            return Err(QueryErrorKind::Authentication);
        }
        let mut variants = vec![DATA_AGENT_VARIANT, TYPED_RESULTS_VARIANT];
        if request.target.stage == QueryStage::Sandbox {
            variants.push(SANDBOX_VARIANT);
        }
        if request.mode == QueryMode::Template {
            variants.push(TEMPLATE_VARIANT);
        }
        let meta = json!({"variants": variants.join(",")});
        let init = json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":protocol::VERSION, "capabilities":{},
            "clientInfo":{"name":"fabric-sdk-aihub-query","version":env!("CARGO_PKG_VERSION")},
            "_meta":meta,
        }});
        let initialized = self
            .exchange(
                &init, &token, None, None, deadline, false, dispatched, canceled,
            )
            .await?;
        let version = protocol::negotiate(&protocol::response(
            &initialized.body,
            &initialized.media,
            1,
        )?)?;
        let session = initialized.session;
        let notification =
            json!({"jsonrpc":"2.0", "method":"notifications/initialized", "params":{"_meta":meta}});
        self.exchange(
            &notification,
            &token,
            session.as_deref(),
            Some(&version),
            deadline,
            false,
            dispatched,
            canceled,
        )
        .await?;
        let mut cursor = None::<String>;
        let mut cursors = BTreeSet::new();
        let mut selected = None;
        let mut next_id = 2_u64;
        for page in 0..self.limits.discovery_pages {
            let mut params = json!({"_meta":meta});
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            let list = json!({"jsonrpc":"2.0","id":next_id,"method":"tools/list","params":params});
            let response = self
                .exchange(
                    &list,
                    &token,
                    session.as_deref(),
                    Some(&version),
                    deadline,
                    false,
                    dispatched,
                    canceled,
                )
                .await?;
            let raw = protocol::response(&response.body, &response.media, next_id)?;
            next_id += 1;
            let tools: protocol::ToolList = protocol::parse(raw.get())?;
            if tools.tools.len() > 1024 {
                return Err(QueryErrorKind::Limit);
            }
            for tool in tools.tools {
                if tool.name == "execute_query" && selected.replace(tool).is_some() {
                    return Err(QueryErrorKind::UnsupportedTool);
                }
            }
            cursor = tools.next_cursor;
            match &cursor {
                None => break,
                Some(cursor)
                    if cursor.is_empty()
                        || cursor.len() > 4096
                        || !cursors.insert(cursor.clone()) =>
                {
                    return Err(QueryErrorKind::Protocol);
                }
                Some(_) if page + 1 == self.limits.discovery_pages => {
                    return Err(QueryErrorKind::Limit);
                }
                _ => {}
            }
        }
        protocol::validate_tool(&selected.ok_or(QueryErrorKind::UnsupportedTool)?, request)?;
        let mut args = json!({"artifactId":request.target.artifact_id, "query":request.query, "max_rows":request.max_rows});
        if let Some(filename) = &request.filename {
            args["filename"] = json!(filename);
        }
        if request.mode == QueryMode::Template {
            args["language"] = json!("jmespath");
        }
        let call = json!({"jsonrpc":"2.0","id":next_id,"method":"tools/call",
            "params":{"name":"execute_query","arguments":args,"_meta":meta}});
        let response = self
            .exchange(
                &call,
                &token,
                session.as_deref(),
                Some(&version),
                deadline,
                true,
                dispatched,
                canceled,
            )
            .await?;
        let raw = protocol::response(&response.body, &response.media, next_id)?;
        let result = decode::decode(&raw, self.limits, request.max_rows as usize, version)?;
        canceled.check().await?;
        deadline.remaining().map_err(|_| QueryErrorKind::Deadline)?;
        Ok(result)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "One bounded MCP exchange with explicit session, version and dispatch evidence."
    )]
    async fn exchange(
        &self,
        message: &Value,
        token: &AccessToken,
        session: Option<&str>,
        version: Option<&str>,
        deadline: &NativeDeadline,
        query: bool,
        dispatched: &AtomicBool,
        canceled: &Cancellation<'_>,
    ) -> Result<Response, QueryErrorKind> {
        let body = serde_json::to_vec(message).map_err(|_| QueryErrorKind::InvalidInput)?;
        if body.len() > 2 * 1024 * 1024 {
            return Err(QueryErrorKind::Limit);
        }
        let mut request = Request::new(self.binding.endpoint.clone(), Method::Post);
        request.insert_header("content-type", "application/json");
        request.insert_header("accept", "application/json, text/event-stream");
        request.insert_header("authorization", format!("Bearer {}", token.token.secret()));
        if let Some(session) = session {
            request.insert_header("mcp-session-id", session.to_owned());
        }
        if let Some(version) = version {
            request.insert_header("mcp-protocol-version", version.to_owned());
        }
        request.set_body(body);
        canceled.check().await?;
        deadline.remaining().map_err(|_| QueryErrorKind::Deadline)?;
        if query {
            dispatched.store(true, Ordering::SeqCst);
        }
        let response = self
            .http
            .execute_request(&request)
            .await
            .map_err(|_| QueryErrorKind::Transport)?;
        let (status, headers, mut stream) = response.deconstruct();
        let status = u16::from(status);
        let notification = message.get("id").is_none();
        if (notification && status != 202) || (!notification && status != 200) {
            return Err(QueryErrorKind::Http(status));
        }
        let mut media = None;
        let mut returned_session = None;
        let mut header_bytes = 0;
        for (index, (name, value)) in headers.iter().enumerate() {
            let value = value.as_str();
            header_bytes += name.as_str().len() + value.len();
            if index >= 64 || header_bytes > 16 * 1024 {
                return Err(QueryErrorKind::Limit);
            }
            if name.as_str().eq_ignore_ascii_case("content-type") {
                media = Some(value.to_owned());
            }
            if name.as_str().eq_ignore_ascii_case("mcp-session-id") {
                if value.is_empty()
                    || value.len() > 256
                    || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
                {
                    return Err(QueryErrorKind::Protocol);
                }
                returned_session = Some(value.to_owned());
            }
        }
        if version.is_some()
            && returned_session
                .as_deref()
                .is_some_and(|id| Some(id) != session)
        {
            return Err(QueryErrorKind::Protocol);
        }
        let mut body = Vec::new();
        let mut chunks = 0;
        while let Some(chunk) = stream.next().await {
            canceled.check().await?;
            deadline.remaining().map_err(|_| QueryErrorKind::Deadline)?;
            let chunk = chunk.map_err(|_| QueryErrorKind::Transport)?;
            if chunk.len() > self.limits.response_bytes.saturating_sub(body.len()) {
                return Err(QueryErrorKind::Limit);
            }
            body.extend_from_slice(&chunk);
            chunks += 1;
            if chunks % 32 == 0 {
                yield_once().await;
            }
        }
        if notification && !body.is_empty() {
            return Err(QueryErrorKind::Protocol);
        }
        Ok(Response {
            body,
            media: if notification {
                String::new()
            } else {
                media.ok_or(QueryErrorKind::Protocol)?
            },
            session: returned_session,
        })
    }
}

struct Response {
    body: Vec<u8>,
    media: String,
    session: Option<String>,
}

struct Cancellation<'a> {
    future: Mutex<Pin<Box<dyn Future<Output = ()> + Send + 'a>>>,
    latched: AtomicBool,
}

impl Cancellation<'_> {
    fn poll(&self, cx: &mut Context<'_>) -> bool {
        if self.latched.load(Ordering::SeqCst) {
            return true;
        }
        let ready = self
            .future
            .lock()
            .expect("private cancellation mutex poisoned")
            .as_mut()
            .poll(cx)
            .is_ready();
        if ready {
            self.latched.store(true, Ordering::SeqCst);
        }
        ready
    }

    async fn check(&self) -> Result<(), QueryErrorKind> {
        poll_fn(|cx| {
            Poll::Ready(if self.poll(cx) {
                Err(QueryErrorKind::Canceled)
            } else {
                Ok(())
            })
        })
        .await
    }
}

async fn yield_once() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}
