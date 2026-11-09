// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-bound native Wildfire and modeled Fabric Livy JSON APIs.
//!
//! Factories and request builders perform no I/O. Execute each request through the
//! caller's existing [`NativeExecutor`]; this module installs no HTTP stack,
//! credential provider, retry, polling loop, async runtime or generic LRO.
//! See `rust/authoring-docs/livy-native.md` for source coverage and integration limitations.

mod models;
mod pages;
mod payload;
mod routes;

pub use models::*;
pub use pages::*;
pub use payload::*;
pub use routes::*;

use crate::runtime::{
    NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod, NativeRequest,
    validate_segment,
};
use serde_json::Value;
use std::{fmt, marker::PhantomData, sync::Arc};

fn protocol_headers(
    request: NativeRequest,
    profile: ServiceProfile,
) -> Result<NativeRequest, NativeError> {
    let request = request.with_header("Accept-Encoding", "identity")?;
    if profile == ServiceProfile::NativeWildfire {
        request.with_header("X-Requested-By", "fabric-sdk-rust-livy")
    } else {
        Ok(request)
    }
}

pub(super) fn segment(value: &str) -> Result<(), NativeError> {
    validate_segment(value)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte))
    {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}

fn numeric_id(id: u64) -> Result<String, NativeError> {
    if id > i64::MAX as u64 {
        return Err(NativeError::InvalidInput);
    }
    Ok(id.to_string())
}

#[derive(Clone)]
struct Binding {
    endpoint: NativeEndpoint,
    profile: ServiceProfile,
}

#[derive(Clone)]
struct Scope {
    binding: Arc<Binding>,
    segments: Vec<String>,
    template: &'static str,
    acquisition: Option<String>,
}

impl Scope {
    fn child(&self, parts: &[&str], template: &'static str) -> Result<Self, NativeError> {
        for part in parts {
            segment(part)?;
        }
        let mut scope = self.clone();
        scope
            .segments
            .extend(parts.iter().map(|part| (*part).into()));
        scope.template = template;
        Ok(scope)
    }

    fn request<T: ResponseModel>(
        &self,
        method: NativeMethod,
        suffix: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        effect: NativeEffect,
    ) -> Result<LivyRequest<T>, NativeError> {
        let template = format!("{}{suffix}", self.template);
        let contract = self
            .binding
            .profile
            .routes()
            .iter()
            .find(|route| route.method == method && route.path == template)
            .ok_or(NativeError::UnsupportedCapability)?;
        if contract.encoding != RequestEncoding::Json {
            return Err(NativeError::UnsupportedCapability);
        }
        let mut parts: Vec<&str> = self.segments.iter().map(String::as_str).collect();
        parts.extend(suffix.split('/').filter(|part| !part.is_empty()));
        let native = self
            .binding
            .endpoint
            .request(method, &parts, query, body, effect)?;
        let native = protocol_headers(native, self.binding.profile)?;
        Ok(LivyRequest {
            native,
            binding: self.binding.clone(),
            acquisition: self.acquisition.clone(),
            contract,
            marker: PhantomData,
        })
    }
}

/// Explicit immutable endpoint/profile binding. Construction does not acquire credentials.
#[derive(Clone)]
pub struct LivyService {
    scope: Scope,
}

impl LivyService {
    /// Bind an already validated native base, including any caller-selected service prefix.
    pub fn native(endpoint: NativeEndpoint) -> Self {
        Self::bound(endpoint, ServiceProfile::NativeWildfire)
    }

    /// Bind a Fabric Lakehouse using `v1` and Livy API `2024-04-01`.
    ///
    /// # Errors
    /// Rejects unsafe workspace/lakehouse IDs before any network or credential access.
    pub fn fabric(
        endpoint: NativeEndpoint,
        workspace_id: &str,
        lakehouse_id: &str,
    ) -> Result<Self, NativeError> {
        Self::fabric_versioned(endpoint, workspace_id, lakehouse_id, "v1", "2024-04-01")
    }

    /// Bind exact caller-selected Fabric/API versions without environment discovery.
    ///
    /// # Errors
    /// Rejects unsafe IDs and versions outside `YYYY-MM-DD[-preview]`.
    pub fn fabric_versioned(
        endpoint: NativeEndpoint,
        workspace_id: &str,
        lakehouse_id: &str,
        fabric_version: &str,
        api_version: &str,
    ) -> Result<Self, NativeError> {
        for value in [workspace_id, lakehouse_id, fabric_version, api_version] {
            segment(value)?;
        }
        let date = api_version.strip_suffix("-preview").unwrap_or(api_version);
        if date.len() != 10
            || !date.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 4 | 7) {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            })
        {
            return Err(NativeError::InvalidInput);
        }
        let base = endpoint.request(
            NativeMethod::Get,
            &[
                fabric_version,
                "workspaces",
                workspace_id,
                "lakehouses",
                lakehouse_id,
                "livyApi",
                "versions",
                api_version,
            ],
            &[],
            None,
            NativeEffect::Read,
        )?;
        Ok(Self::bound(
            NativeEndpoint::new(base.url().clone())?,
            ServiceProfile::FabricJson,
        ))
    }

    fn bound(endpoint: NativeEndpoint, profile: ServiceProfile) -> Self {
        Self {
            scope: Scope {
                binding: Arc::new(Binding { endpoint, profile }),
                segments: Vec::new(),
                template: "",
                acquisition: None,
            },
        }
    }

    /// The explicitly selected source profile.
    pub fn profile(&self) -> ServiceProfile {
        self.scope.binding.profile
    }

    /// Native service version metadata. Fabric does not advertise this route.
    ///
    /// # Errors
    /// Returns `UnsupportedCapability` for Fabric.
    pub fn version(&self) -> Result<LivyRequest<ServiceMetadata>, NativeError> {
        self.scope
            .request(NativeMethod::Get, "/version", &[], None, NativeEffect::Read)
    }

    /// Build a session-list request using only the selected profile's parameters.
    ///
    /// # Errors
    /// Rejects cross-profile or out-of-range pagination.
    pub fn list_sessions(
        &self,
        page: ActivityPage,
    ) -> Result<LivyRequest<ActivityList>, NativeError> {
        let query =
            payload::activity_query(page, self.profile() == ServiceProfile::NativeWildfire)?;
        self.scope.request(
            NativeMethod::Get,
            "/sessions",
            &query,
            None,
            NativeEffect::Read,
        )
    }

    /// Request classic session creation, retaining every configuration field.
    ///
    /// # Errors
    /// Returns shared request-validation errors without dispatch.
    pub fn create_session(
        &self,
        input: &Configuration,
    ) -> Result<LivyRequest<SessionSnapshot>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/sessions",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Create,
        )
    }

    /// Construct a classic session handle without reading or creating remote infrastructure.
    ///
    /// # Errors
    /// Rejects unsafe opaque IDs (numeric IDs can be supplied as decimal strings).
    pub fn session(&self, id: &str) -> Result<Session, NativeError> {
        Ok(Session {
            scope: self.scope.child(&["sessions", id], "/sessions/{id}")?,
        })
    }

    /// Request HC acquisition, not creation of a privately owned backing Spark session.
    ///
    /// # Errors
    /// Rejects native profiles and wrong known HC schema field types.
    pub fn acquire_high_concurrency(
        &self,
        input: &Configuration,
    ) -> Result<LivyRequest<SessionSnapshot>, NativeError> {
        input.validate_hc()?;
        self.scope.request(
            NativeMethod::Post,
            "/highConcurrencySessions",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Create,
        )
    }

    /// Construct a Fabric HC management handle using the acquisition ID only.
    ///
    /// # Errors
    /// Rejects native profiles and unsafe acquisition IDs.
    pub fn high_concurrency(&self, acquisition_id: &str) -> Result<HighConcurrency, NativeError> {
        if self.profile() != ServiceProfile::FabricJson {
            return Err(NativeError::UnsupportedCapability);
        }
        let mut scope = self.scope.child(
            &["highConcurrencySessions", acquisition_id],
            "/highConcurrencySessions/{id}",
        )?;
        scope.acquisition = Some(acquisition_id.into());
        Ok(HighConcurrency { scope })
    }

    /// Request a batch list without translating native and Fabric pagination.
    ///
    /// # Errors
    /// Rejects cross-profile or out-of-range pagination.
    pub fn list_batches(
        &self,
        page: ActivityPage,
    ) -> Result<LivyRequest<ActivityList>, NativeError> {
        let query =
            payload::activity_query(page, self.profile() == ServiceProfile::NativeWildfire)?;
        self.scope.request(
            NativeMethod::Get,
            "/batches",
            &query,
            None,
            NativeEffect::Read,
        )
    }

    /// Submit a batch Spark application, not an idle resource shell.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn create_batch(
        &self,
        input: &Configuration,
    ) -> Result<LivyRequest<BatchSnapshot>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/batches",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Execute,
        )
    }

    /// Construct a batch handle without I/O.
    ///
    /// # Errors
    /// Rejects unsafe batch IDs.
    pub fn batch(&self, id: &str) -> Result<Batch, NativeError> {
        Ok(Batch {
            scope: self.scope.child(&["batches", id], "/batches/{id}")?,
        })
    }
}

macro_rules! handle {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone)]
        pub struct $name {
            scope: Scope,
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<owner-bound>)"))
            }
        }
    };
}

handle!(
    Session,
    "An exact classic session owner; never an HC backing-session cleanup handle."
);
handle!(
    HighConcurrency,
    "A Fabric HC acquisition management owner, distinct from backing session/REPL IDs."
);
handle!(
    Repl,
    "An exact native session/REPL or Fabric acquisition/backing-session/REPL tuple."
);
handle!(
    Statements,
    "Statement collection bound to exactly one classic session or owned REPL."
);
handle!(
    Statement,
    "Statement handle retaining its full service, session, REPL and acquisition ownership."
);
handle!(Batch, "An exact batch application owner.");
handle!(
    Job,
    "Serialized job handle bound to one native classic session."
);

macro_rules! request_method {
    ($name:ident, $method:ident, $suffix:literal, $model:ty, $effect:ident, $doc:literal) => {
        #[doc = $doc]
        ///
        /// # Errors
        /// Returns `UnsupportedCapability` if absent from the selected profile.
        pub fn $name(&self) -> Result<LivyRequest<$model>, NativeError> {
            self.scope.request(
                NativeMethod::$method,
                $suffix,
                &[],
                None,
                NativeEffect::$effect,
            )
        }
    };
}

impl Session {
    request_method!(
        get,
        Get,
        "",
        SessionSnapshot,
        Read,
        "Read this exact classic session."
    );
    request_method!(
        delete,
        Delete,
        "",
        Acknowledgement,
        Delete,
        "Explicitly delete the classic session, not a local handle close."
    );
    request_method!(
        get_state,
        Get,
        "/state",
        SessionSnapshot,
        Read,
        "Read the native state subresource."
    );
    request_method!(
        stop,
        Post,
        "/stop",
        Acknowledgement,
        Lifecycle,
        "Native stop, distinct from DELETE and statement cancellation."
    );
    request_method!(
        interrupt,
        Post,
        "/interrupt",
        CancellationResult,
        Lifecycle,
        "Native interrupt; qualified Wildfire stops the whole session, not one statement."
    );
    request_method!(
        connect,
        Post,
        "/connect",
        SessionSnapshot,
        Lifecycle,
        "Native remote connect/attach, not a local resume."
    );
    request_method!(
        get_timeout,
        Get,
        "/timeout",
        Timeout,
        Read,
        "Read native timeout settings and inherited values."
    );

    /// Reset activity using native PUT or modeled Fabric POST.
    ///
    /// # Errors
    /// Returns shared request-validation errors. No generic LRO or completion is implied.
    pub fn reset_timeout(&self) -> Result<LivyRequest<SessionTimeoutReset>, NativeError> {
        let method = match self.scope.binding.profile {
            ServiceProfile::NativeWildfire => NativeMethod::Put,
            ServiceProfile::FabricJson => NativeMethod::Post,
        };
        self.scope
            .request(method, "/reset-timeout", &[], None, NativeEffect::Lifecycle)
    }

    /// Read one native log page.
    ///
    /// # Errors
    /// Fabric does not advertise this route.
    pub fn logs(&self, page: BytePage) -> Result<LivyRequest<Logs>, NativeError> {
        self.scope.request(
            NativeMethod::Get,
            "/log",
            &page.query(),
            None,
            NativeEffect::Read,
        )
    }

    /// Set a native classic session duration, not its REPL duration.
    ///
    /// # Errors
    /// Rejects REPL fields and unsupported profiles before dispatch.
    pub fn set_timeout(&self, input: &TimeoutInput) -> Result<LivyRequest<Timeout>, NativeError> {
        if input.as_json().get("replTimeout").is_some() {
            return Err(NativeError::InvalidInput);
        }
        self.scope.request(
            NativeMethod::Put,
            "/timeout",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Update,
        )
    }

    /// Native completion; sourceId, if supplied, is explicit in the payload.
    ///
    /// # Errors
    /// Fabric does not advertise this extension.
    pub fn completion(
        &self,
        input: &CompletionInput,
    ) -> Result<LivyRequest<Completion>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/completion",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Read,
        )
    }

    /// Native variable inspection.
    ///
    /// # Errors
    /// Fabric does not advertise this extension.
    pub fn inspect(&self, input: &InspectionInput) -> Result<LivyRequest<Inspection>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/inspect",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Read,
        )
    }

    /// Native default-REPL debugging; no source-ID override is invented.
    ///
    /// # Errors
    /// Fabric does not advertise this extension.
    pub fn debug(&self, input: &DebugInput) -> Result<LivyRequest<OpaqueJson>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/debug",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Execute,
        )
    }

    /// Localize a resource URI on the server without fetching it in the SDK.
    ///
    /// # Errors
    /// Fabric does not advertise native resource loading.
    pub fn add_resource(
        &self,
        kind: ResourceKind,
        uri: &ResourceUri,
    ) -> Result<LivyRequest<Acknowledgement>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            kind.add_suffix(),
            &[],
            Some(uri.as_json().clone()),
            NativeEffect::Update,
        )
    }

    /// Describe a binary upload without opening a file or consuming its content.
    ///
    /// # Errors
    /// Rejects Fabric, unsafe filenames and unsupported lengths.
    pub fn upload_descriptor(
        &self,
        kind: ResourceKind,
        filename: &str,
        content_length: u64,
    ) -> Result<UploadDescriptor, NativeError> {
        if self.scope.binding.profile != ServiceProfile::NativeWildfire {
            return Err(NativeError::UnsupportedCapability);
        }
        if filename.is_empty()
            || matches!(filename, "." | "..")
            || filename.contains(['/', '\\'])
            || filename.chars().any(char::is_control)
        {
            return Err(NativeError::InvalidInput);
        }
        let template = format!("{}{}", self.scope.template, kind.upload_suffix());
        let contract = NATIVE_ROUTES
            .iter()
            .find(|route| route.path == template)
            .ok_or(NativeError::UnsupportedCapability)?;
        if content_length > i64::MAX as u64 {
            return Err(NativeError::InvalidInput);
        }
        let mut segments: Vec<&str> = self.scope.segments.iter().map(String::as_str).collect();
        segments.push(kind.upload_suffix().trim_start_matches('/'));
        let native = self.scope.binding.endpoint.request(
            NativeMethod::Post,
            &segments,
            &[],
            None,
            NativeEffect::Update,
        )?;
        let native = protocol_headers(native, self.scope.binding.profile)?;
        Ok(UploadDescriptor {
            native,
            binding: self.scope.binding.clone(),
            contract,
            kind,
            filename: filename.into(),
            content_length,
        })
    }

    /// Construct a native REPL owner without creating it.
    ///
    /// # Errors
    /// Fabric classic REPL extensions are not modeled; unsafe IDs are rejected.
    pub fn repl(&self, repl_id: &str) -> Result<Repl, NativeError> {
        if self.scope.binding.profile != ServiceProfile::NativeWildfire {
            return Err(NativeError::UnsupportedCapability);
        }
        Ok(Repl {
            scope: self
                .scope
                .child(&["repls", repl_id], "/sessions/{id}/repls/{replId}")?,
        })
    }

    /// List native REPLs with paired offset/take and optional literal filter.
    ///
    /// # Errors
    /// Fabric does not advertise this route.
    pub fn list_repls(
        &self,
        options: &ReplListOptions,
    ) -> Result<LivyRequest<ActivityList>, NativeError> {
        self.scope.request(
            NativeMethod::Get,
            "/repls",
            &options.query(),
            None,
            NativeEffect::Read,
        )
    }

    /// Construct this classic session's statement collection without I/O.
    pub fn statements(&self) -> Statements {
        Statements {
            scope: self.scope.clone(),
        }
    }

    /// Submit an already serialized native job through `submit-job`, without replay.
    ///
    /// # Errors
    /// Fabric does not advertise serialized native jobs.
    pub fn submit_job(
        &self,
        input: &SerializedJob,
    ) -> Result<LivyRequest<JobSnapshot>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/submit-job",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Execute,
        )
    }

    /// Run an already serialized native job through the distinct `run-job` route.
    ///
    /// # Errors
    /// Fabric does not advertise serialized native jobs.
    pub fn run_job(&self, input: &SerializedJob) -> Result<LivyRequest<JobSnapshot>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/run-job",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Execute,
        )
    }

    /// Construct an exact native job handle, not a statement or scheduler job.
    ///
    /// # Errors
    /// Rejects Fabric and IDs above the signed wire range.
    pub fn job(&self, job_id: u64) -> Result<Job, NativeError> {
        if self.scope.binding.profile != ServiceProfile::NativeWildfire {
            return Err(NativeError::UnsupportedCapability);
        }
        Ok(Job {
            scope: self.scope.child(
                &["jobs", &numeric_id(job_id)?],
                "/sessions/{id}/jobs/{jobId}",
            )?,
        })
    }
}

impl HighConcurrency {
    request_method!(
        get,
        Get,
        "",
        SessionSnapshot,
        Read,
        "Read this acquisition management identity only."
    );
    request_method!(
        release,
        Delete,
        "",
        Acknowledgement,
        Delete,
        "Release this owned acquisition, never delete shared backing Spark."
    );

    /// Construct a Fabric execution owner using an explicit backing session ID and REPL ID.
    ///
    /// The retained acquisition ID never substitutes for the backing session ID in the wire route.
    ///
    /// # Errors
    /// Rejects unsafe IDs. No backing-session management/delete handle is exposed.
    pub fn repl(&self, backing_session_id: &str, repl_id: &str) -> Result<Repl, NativeError> {
        segment(backing_session_id)?;
        segment(repl_id)?;
        let mut scope = self.scope.clone();
        scope.segments = vec![
            "highConcurrencySessions".into(),
            backing_session_id.into(),
            "repls".into(),
            repl_id.into(),
        ];
        scope.template = "/highConcurrencySessions/{id}/repls/{replId}";
        Ok(Repl { scope })
    }

    /// Read the management acquisition ID without conflating it with a backing session.
    pub fn acquisition_id(&self) -> &str {
        &self.scope.segments[1]
    }
}

impl Repl {
    request_method!(
        get,
        Get,
        "",
        SessionSnapshot,
        Read,
        "Read an owned native REPL; Fabric HC only models statement routes."
    );
    request_method!(
        delete,
        Delete,
        "",
        Acknowledgement,
        Delete,
        "Delete exactly the native REPL, not its backing session."
    );
    request_method!(
        stop,
        Post,
        "/stop",
        Acknowledgement,
        Lifecycle,
        "Stop exactly the native REPL."
    );
    request_method!(
        reset_timeout,
        Put,
        "/reset-timeout",
        CancellationResult,
        Lifecycle,
        "Reset native REPL activity, not timeout duration."
    );
    request_method!(
        get_timeout,
        Get,
        "/timeout",
        Timeout,
        Read,
        "Read native REPL/session timeout values."
    );

    /// Request native PUT REPL creation with unchanged deployment configuration.
    ///
    /// # Errors
    /// Fabric HC does not advertise native REPL management.
    pub fn create(
        &self,
        input: &Configuration,
    ) -> Result<LivyRequest<SessionSnapshot>, NativeError> {
        self.scope.request(
            NativeMethod::Put,
            "",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Create,
        )
    }

    /// Set one or both positive native timeout durations.
    ///
    /// # Errors
    /// Fabric HC does not advertise native REPL management.
    pub fn set_timeout(&self, input: &TimeoutInput) -> Result<LivyRequest<Timeout>, NativeError> {
        self.scope.request(
            NativeMethod::Put,
            "/timeout",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Update,
        )
    }

    /// Construct a statement collection preserving the full owner tuple.
    pub fn statements(&self) -> Statements {
        Statements {
            scope: self.scope.clone(),
        }
    }

    /// Management acquisition ID, only for an HC execution owner.
    pub fn acquisition_id(&self) -> Option<&str> {
        self.scope.acquisition.as_deref()
    }

    /// Exact backing session ID (not the management acquisition ID).
    pub fn backing_session_id(&self) -> &str {
        &self.scope.segments[1]
    }

    /// Exact REPL ID.
    pub fn repl_id(&self) -> &str {
        &self.scope.segments[3]
    }
}

impl Statements {
    /// List statements. Fabric models no list paging parameters.
    ///
    /// # Errors
    /// Rejects nonempty native list options on Fabric before dispatch.
    pub fn list(
        &self,
        options: StatementListOptions,
    ) -> Result<LivyRequest<ActivityList>, NativeError> {
        let query = options.query();
        if self.scope.binding.profile == ServiceProfile::FabricJson && !query.is_empty() {
            return Err(NativeError::UnsupportedCapability);
        }
        self.scope.request(
            NativeMethod::Get,
            "/statements",
            &query,
            None,
            NativeEffect::Read,
        )
    }

    /// Execute explicit code in this owner. No implicit sourceId or new runtime.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn submit(
        &self,
        input: &StatementInput,
    ) -> Result<LivyRequest<StatementSnapshot>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/statements",
            &[],
            Some(input.as_json().clone()),
            NativeEffect::Execute,
        )
    }

    /// Construct an exact statement handle without performing a read.
    ///
    /// # Errors
    /// Rejects IDs above the signed wire integer range.
    pub fn statement(&self, id: u64) -> Result<Statement, NativeError> {
        let template = match self.scope.template {
            "/sessions/{id}" => "/sessions/{id}/statements/{statementId}",
            "/sessions/{id}/repls/{replId}" => {
                "/sessions/{id}/repls/{replId}/statements/{statementId}"
            }
            "/highConcurrencySessions/{id}/repls/{replId}" => {
                "/highConcurrencySessions/{id}/repls/{replId}/statements/{statementId}"
            }
            _ => return Err(NativeError::InvalidInput),
        };
        Ok(Statement {
            scope: self
                .scope
                .child(&["statements", &numeric_id(id)?], template)?,
        })
    }
}

impl Statement {
    /// Read one exact JSON result fragment. `from`/`size` are bytes, not rows.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn get(&self, page: BytePage) -> Result<LivyRequest<StatementSnapshot>, NativeError> {
        self.scope.request(
            NativeMethod::Get,
            "",
            &page.query(),
            None,
            NativeEffect::Read,
        )
    }

    /// Request cancellation once with an explicit empty JSON object on both profiles.
    ///
    /// # Errors
    /// Returns shared request-validation errors. Acknowledgement is never termination proof.
    pub fn cancel(&self) -> Result<LivyRequest<CancellationResult>, NativeError> {
        self.scope.request(
            NativeMethod::Post,
            "/cancel",
            &[],
            Some(serde_json::json!({})),
            NativeEffect::Cancel,
        )
    }
}

impl Batch {
    request_method!(
        get,
        Get,
        "",
        BatchSnapshot,
        Read,
        "Read exactly this batch application."
    );
    request_method!(
        cancel,
        Delete,
        "",
        Acknowledgement,
        Cancel,
        "Terminate this batch with the source DELETE route."
    );
    request_method!(
        get_state,
        Get,
        "/state",
        BatchSnapshot,
        Read,
        "Read native batch state."
    );
    request_method!(
        reset_timeout,
        Put,
        "/reset-timeout",
        Acknowledgement,
        Lifecycle,
        "Reset native batch activity."
    );

    /// Read one native batch log page.
    ///
    /// # Errors
    /// Fabric does not advertise this extension.
    pub fn logs(&self, page: BytePage) -> Result<LivyRequest<Logs>, NativeError> {
        self.scope.request(
            NativeMethod::Get,
            "/log",
            &page.query(),
            None,
            NativeEffect::Read,
        )
    }
}

impl Job {
    request_method!(
        get,
        Get,
        "",
        JobSnapshot,
        Read,
        "Read this exact serialized native job."
    );
    request_method!(
        cancel,
        Post,
        "/cancel",
        Acknowledgement,
        Cancel,
        "Request cancellation; native cancellation can remove the job handle."
    );
}

/// Runtime resource localization kind; no arbitrary route suffix is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    /// JVM archive, with multipart part name `jar`.
    Jar,
    /// Python file, with multipart part name `file`.
    PyFile,
    /// Ordinary file, with multipart part name `file`.
    File,
}

impl ResourceKind {
    fn add_suffix(self) -> &'static str {
        match self {
            Self::Jar => "/add-jar",
            Self::PyFile => "/add-pyfile",
            Self::File => "/add-file",
        }
    }

    fn upload_suffix(self) -> &'static str {
        match self {
            Self::Jar => "/upload-jar",
            Self::PyFile => "/upload-pyfile",
            Self::File => "/upload-file",
        }
    }
}

/// Native binary upload descriptor. No file is opened and no bytes are dispatched.
#[derive(Clone)]
pub struct UploadDescriptor {
    native: NativeRequest,
    binding: Arc<Binding>,
    contract: &'static RouteContract,
    kind: ResourceKind,
    filename: String,
    content_length: u64,
}

impl UploadDescriptor {
    /// Exact native multipart source operation.
    pub const fn contract(&self) -> &'static RouteContract {
        self.contract
    }

    /// Scoped endpoint path; not a new caller-supplied URL.
    pub fn url(&self) -> &azure_core::http::Url {
        self.native.url()
    }

    /// Multipart field name: `jar` for JARs, `file` otherwise.
    pub const fn part_name(&self) -> &'static str {
        match self.kind {
            ResourceKind::Jar => "jar",
            ResourceKind::PyFile | ResourceKind::File => "file",
        }
    }

    /// Validated basename supplied by the caller.
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Explicit caller-known payload length.
    pub const fn content_length(&self) -> u64 {
        self.content_length
    }

    /// Native binary multipart part media type.
    pub const fn content_type(&self) -> &'static str {
        "application/octet-stream"
    }

    /// Build a bounded binary multipart request using caller-supplied bytes and boundary.
    ///
    /// The complete encoded body, not just file bytes, must fit the supplied limit.
    /// No stream/file is opened, content is never serialized as JSON/base64, and
    /// no network access occurs. Streaming uploads remain a separate host capability.
    ///
    /// # Errors
    /// Rejects length mismatch, an unsafe/colliding boundary or an exceeded bound.
    pub fn into_multipart_request(
        self,
        content: Vec<u8>,
        boundary: &str,
        max_request_bytes: usize,
    ) -> Result<LivyRequest<Acknowledgement>, NativeError> {
        if boundary.is_empty()
            || boundary.len() > 70
            || !boundary
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || u64::try_from(content.len()).map_err(|_| NativeError::Limit)? != self.content_length
            || max_request_bytes == 0
        {
            return Err(NativeError::InvalidInput);
        }
        if content.len() > max_request_bytes {
            return Err(NativeError::Limit);
        }
        let delimiter = format!("--{boundary}");
        if content
            .windows(delimiter.len())
            .any(|window| window == delimiter.as_bytes())
        {
            return Err(NativeError::InvalidInput);
        }
        let prefix = format!(
            "{delimiter}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"",
            self.part_name(),
        );
        let header_end = b"\"\r\nContent-Type: application/octet-stream\r\n\r\n";
        let suffix = format!("\r\n{delimiter}--\r\n");
        let escaped_name_len = self
            .filename
            .bytes()
            .filter(|byte| *byte == b'"')
            .count()
            .checked_mul(2)
            .and_then(|extra| extra.checked_add(self.filename.len()))
            .ok_or(NativeError::Limit)?;
        let length = [
            prefix.len(),
            escaped_name_len,
            header_end.len(),
            content.len(),
            suffix.len(),
        ]
        .into_iter()
        .try_fold(0usize, |total, length| total.checked_add(length))
        .ok_or(NativeError::Limit)?;
        if length > max_request_bytes {
            return Err(NativeError::Limit);
        }
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(prefix.as_bytes());
        for byte in self.filename.bytes() {
            if byte == b'"' {
                bytes.extend_from_slice(b"%22");
            } else {
                bytes.push(byte);
            }
        }
        bytes.extend_from_slice(header_end);
        bytes.extend_from_slice(&content);
        bytes.extend_from_slice(suffix.as_bytes());
        let native = self.native.with_bytes(
            bytes,
            &format!("multipart/form-data; boundary={boundary}"),
            max_request_bytes,
        )?;
        Ok(LivyRequest {
            native,
            binding: self.binding,
            acquisition: None,
            contract: self.contract,
            marker: PhantomData,
        })
    }

    /// Refuse to disguise binary multipart as a JSON request.
    ///
    /// # Errors
    /// Always returns `UnsupportedCapability`: use the explicit multipart byte representation.
    pub fn into_json_request(self) -> Result<LivyRequest<Acknowledgement>, NativeError> {
        Err(NativeError::UnsupportedCapability)
    }
}

impl fmt::Debug for UploadDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadDescriptor")
            .field("kind", &self.kind)
            .field("content_length", &self.content_length)
            .field("owner", &"<scoped>")
            .finish()
    }
}

impl fmt::Debug for LivyService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LivyService")
            .field("profile", &self.profile())
            .field("endpoint", &"<caller-selected>")
            .finish()
    }
}

/// One validated, typed Livy operation. No dispatch occurs until explicitly executed.
pub struct LivyRequest<T: ResponseModel> {
    native: NativeRequest,
    binding: Arc<Binding>,
    acquisition: Option<String>,
    contract: &'static RouteContract,
    marker: PhantomData<T>,
}

/// Whether HTTP returned a representation or merely accepted an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseDisposition {
    /// A native response was returned. This is not a workload-success claim.
    Returned,
    /// HTTP 202: accepted, without a generic LRO or terminal-state assumption.
    Accepted,
}

/// A bounded native response retaining both immutable JSON and exact wire bytes.
pub struct LivyResponse<T: ResponseModel> {
    request: NativeRequest,
    binding: Arc<Binding>,
    acquisition: Option<String>,
    status: u16,
    raw: Vec<u8>,
    content_type: Option<String>,
    model: Option<T>,
}

/// Failure stage, separate from workload state/output errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailurePhase {
    /// Rejected before executor invocation.
    Validation,
    /// Caller-owned executor returned a transport/deadline/cancellation error.
    Execution,
    /// Actual non-success HTTP status.
    Http,
    /// Malformed representation or unexpected empty response.
    Decode,
    /// Caller/executor response bound exceeded.
    ResponseLimit,
}

/// Sanitized request failure, with explicitly accessible bounded provider evidence.
pub struct LivyError {
    source: NativeError,
    phase: FailurePhase,
    status: Option<u16>,
    raw: Vec<u8>,
    outcome_unknown: bool,
}

impl LivyError {
    /// Shared native error, preserving an actual HTTP status when supplied.
    pub const fn native_error(&self) -> NativeError {
        self.source
    }
    /// Failure stage, not a workload execution state.
    pub const fn phase(&self) -> FailurePhase {
        self.phase
    }
    /// Actual HTTP status if a response was received.
    pub const fn status(&self) -> Option<u16> {
        self.status
    }
    /// Bounded raw service evidence. Never automatically logged or displayed.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }
    /// A dispatched mutation may have applied; do not replay it.
    pub const fn outcome_unknown(&self) -> bool {
        self.outcome_unknown
    }
}

impl fmt::Debug for LivyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LivyError")
            .field("source", &self.source)
            .field("phase", &self.phase)
            .field("status", &self.status)
            .field("outcome_unknown", &self.outcome_unknown)
            .field("raw", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for LivyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Livy request failed: {}", self.source)
    }
}

impl std::error::Error for LivyError {}

impl<T: ResponseModel> LivyRequest<T> {
    /// Inspect the validated shared request, including exact payload/parameters.
    pub fn native(&self) -> &NativeRequest {
        &self.native
    }
    /// Pinned source operation and response schema.
    pub const fn contract(&self) -> &'static RouteContract {
        self.contract
    }
    /// Explicit service profile.
    pub fn profile(&self) -> ServiceProfile {
        self.binding.profile
    }
    /// Retained HC management owner, never substituted into execution paths.
    pub fn acquisition_id(&self) -> Option<&str> {
        self.acquisition.as_deref()
    }

    /// Execute once through the caller's existing stack, with no retry, polling or LRO.
    ///
    /// The executor owns authorization binding, redirects, deadlines, cancellation,
    /// streamed byte enforcement and host-controlled security headers.
    ///
    /// # Errors
    /// Returns sanitized execution/HTTP/representation failures. A dispatched mutation
    /// failure is conservatively outcome-unknown; retain this request/owner for reconciliation.
    pub async fn execute(
        &self,
        executor: &dyn NativeExecutor,
        max_response_bytes: usize,
    ) -> Result<LivyResponse<T>, LivyError> {
        let mutation = self.native.method() != NativeMethod::Get;
        let error = |source, phase, status, raw, dispatched| LivyError {
            source,
            phase,
            status,
            raw,
            outcome_unknown: dispatched && mutation,
        };
        if max_response_bytes == 0 {
            return Err(error(
                NativeError::InvalidInput,
                FailurePhase::Validation,
                None,
                Vec::new(),
                false,
            ));
        }
        let response = self
            .native
            .execute(executor, max_response_bytes)
            .await
            .map_err(|source| {
                let phase = if source == NativeError::Limit {
                    FailurePhase::ResponseLimit
                } else {
                    FailurePhase::Execution
                };
                error(source, phase, None, Vec::new(), true)
            })?;
        if !(200..300).contains(&response.status) {
            return Err(error(
                NativeError::Http(response.status),
                FailurePhase::Http,
                Some(response.status),
                response.body,
                true,
            ));
        }
        let acknowledgement = self.contract.response_schema == "Acknowledgement";
        let is_json = response.content_type.as_deref().is_some_and(|media| {
            media
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        });
        let parsed = if acknowledgement {
            T::decode_acknowledgement(if is_json {
                serde_json::from_slice(&response.body).unwrap_or(Value::Null)
            } else {
                Value::Null
            })
            .map(Some)
        } else if response.status == 202 && response.body.is_empty() {
            Ok(None)
        } else if !is_json {
            Err(NativeError::InvalidResponse)
        } else {
            serde_json::from_slice(&response.body)
                .map_err(|_| NativeError::InvalidResponse)
                .and_then(|value: Value| {
                    if value.is_object() {
                        T::decode(value)
                    } else {
                        Err(NativeError::InvalidResponse)
                    }
                })
                .map(Some)
        };
        let model = parsed.map_err(|source| {
            error(
                source,
                FailurePhase::Decode,
                Some(response.status),
                response.body.clone(),
                true,
            )
        })?;
        Ok(LivyResponse {
            request: self.native.clone(),
            binding: self.binding.clone(),
            acquisition: self.acquisition.clone(),
            status: response.status,
            raw: response.body,
            content_type: response.content_type,
            model,
        })
    }
}

impl<T: ResponseModel> LivyResponse<T> {
    /// Actual native HTTP status.
    pub const fn status(&self) -> u16 {
        self.status
    }
    /// HTTP acceptance is separate from any model's workload observation.
    pub const fn disposition(&self) -> ResponseDisposition {
        if self.status == 202 {
            ResponseDisposition::Accepted
        } else {
            ResponseDisposition::Returned
        }
    }
    /// Exact immutable response bytes, including text acknowledgements and opaque results.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }
    /// Original provider media type.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
    /// Parsed model, absent for an empty HTTP 202 response.
    pub fn model(&self) -> Option<&T> {
        self.model.as_ref()
    }
    /// Exact original validated request for explicit owner-bound reconciliation.
    pub fn request(&self) -> &NativeRequest {
        &self.request
    }
    /// No offline/source-only response is proof of general deployed capability.
    pub const fn maturity(&self) -> Maturity {
        Maturity::SourceModeledDeploymentUnverified
    }
}

impl<T: ResponseModel> fmt::Debug for LivyRequest<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LivyRequest")
            .field("source_id", &self.contract.source_id)
            .field("request", &self.native)
            .finish()
    }
}

impl<T: ResponseModel> fmt::Debug for LivyResponse<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LivyResponse")
            .field("status", &self.status)
            .field("body", &"<redacted>")
            .finish()
    }
}
