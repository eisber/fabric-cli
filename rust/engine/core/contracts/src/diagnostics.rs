// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded diagnostic metadata. No event can carry arbitrary text, errors,
//! credentials, bodies, operation handles, or recovery information.

use crate::{CoreError, ErrorKind};
use async_trait::async_trait;
use std::{fmt, time::Duration};

macro_rules! labels {
    ($(#[$meta:meta])* pub enum $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $label:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $(Self::$variant => $label),+ })
            }
        }
    };
}

labels! {
    /// CLI stages; command names remain the responsibility of the CLI.
    pub enum DiagnosticStage {
        /// Overall command execution.
        Command => "Command",
        /// Input validation and preparation.
        Prepare => "Preparation",
        /// Selected-session resolution.
        Selection => "Session selection",
        /// Explicit authentication work.
        Authentication => "Authentication",
        /// Output projection.
        Projection => "Projection"
    }
}

/// Fixed lifecycle categories without arbitrary error detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticPhase {
    /// Work is beginning.
    Started,
    /// Work completed successfully.
    Completed,
    /// Work stopped with a local safe category.
    Failed(ErrorKind),
}
impl fmt::Display for DiagnosticPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed(_) => "failed",
        })
    }
}

/// Duration in milliseconds, saturated rather than overflowing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticDuration(u64);
impl DiagnosticDuration {
    /// Convert a clock duration into bounded metadata.
    pub fn from_duration(duration: Duration) -> Self {
        Self(duration.as_millis().min(u128::from(u64::MAX)) as u64)
    }
    /// Milliseconds suitable for deterministic rendering.
    pub fn millis(self) -> u64 {
        self.0
    }
}
impl fmt::Display for DiagnosticDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ms", self.0)
    }
}

labels! {
    /// Allowed methods, never a caller-provided string.
    pub enum DiagnosticMethod {
        /// Read.
        Get => "GET",
        /// Create or invoke.
        Post => "POST",
        /// Replace.
        Put => "PUT",
        /// Modify.
        Patch => "PATCH",
        /// Remove.
        Delete => "DELETE"
    }
}
labels! {
    /// Only known public origins or a fixed advertised-origin label.
    pub enum DiagnosticOrigin {
        /// Fabric public origin.
        Fabric => "https://api.fabric.microsoft.com",
        /// OneLake public origin.
        Storage => "https://onelake.dfs.fabric.microsoft.com",
        /// ARM public origin.
        Azure => "https://management.azure.com",
        /// Power BI public origin.
        PowerBi => "https://api.powerbi.com",
        /// Validated nonstandard origin, with its hostname withheld.
        ServiceAdvertised => "service-advertised"
    }
}
labels! {
    /// Finite route templates. Every variable segment is withheld.
    pub enum DiagnosticRoute {
        /// Unrecognized path.
        Redacted => "/<redacted>",
        /// Service root.
        Root => "/",
        /// Fabric root.
        FabricRoot => "/v1",
        /// Workspace collection.
        Workspaces => "/v1/workspaces",
        /// A workspace.
        Workspace => "/v1/workspaces/{workspaceId}",
        /// Item collection.
        Items => "/v1/workspaces/{workspaceId}/items",
        /// An item.
        Item => "/v1/workspaces/{workspaceId}/items/{itemId}",
        /// Item jobs.
        JobInstances => "/v1/workspaces/{workspaceId}/items/{itemId}/jobs/instances",
        /// A job instance.
        JobInstance => "/v1/workspaces/{workspaceId}/items/{itemId}/jobs/instances/{jobId}",
        /// Fabric operation status.
        Operation => "/v1/operations/{operationId}",
        /// Fabric operation result.
        OperationResult => "/v1/operations/{operationId}/result",
        /// Fabric capacities.
        Capacities => "/v1/capacities",
        /// A Fabric capacity.
        Capacity => "/v1/capacities/{capacityId}",
        /// ARM subscriptions.
        Subscriptions => "/subscriptions",
        /// A subscription.
        Subscription => "/subscriptions/{subscriptionId}",
        /// ARM resource groups.
        ResourceGroups => "/subscriptions/{subscriptionId}/resourceGroups",
        /// A resource group.
        ResourceGroup => "/subscriptions/{subscriptionId}/resourceGroups/{resourceGroupId}",
        /// Power BI root.
        PowerBiRoot => "/v1.0/myorg",
        /// Power BI groups.
        Groups => "/v1.0/myorg/groups",
        /// A Power BI group.
        Group => "/v1.0/myorg/groups/{groupId}"
    }
}

/// A URL represented solely by fixed origin and route labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeUrl {
    origin: DiagnosticOrigin,
    route: DiagnosticRoute,
}
impl SafeUrl {
    /// Construct from fixed labels only, after validating the source URL.
    pub fn new(origin: DiagnosticOrigin, route: DiagnosticRoute) -> Self {
        Self { origin, route }
    }
    /// Fixed origin label.
    pub fn origin(self) -> DiagnosticOrigin {
        self.origin
    }
    /// Fixed route label.
    pub fn route(self) -> DiagnosticRoute {
        self.route
    }
}
impl fmt::Display for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.origin, self.route)
    }
}

/// Canonical, lower-case, hyphenated UUID request identifier.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RequestId([u8; 36]);
impl RequestId {
    /// Accept only exactly 36 ASCII UUID characters, without trimming.
    pub fn parse(value: &str) -> Option<Self> {
        let mut bytes: [u8; 36] = value.as_bytes().try_into().ok()?;
        for (index, byte) in bytes.iter_mut().enumerate() {
            if [8, 13, 18, 23].contains(&index) {
                if *byte != b'-' {
                    return None;
                }
            } else if byte.is_ascii_hexdigit() {
                *byte = byte.to_ascii_lowercase();
            } else {
                return None;
            }
        }
        Some(Self(bytes))
    }
    /// Full validated ID.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("canonical UUID is ASCII")
    }
}
impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl fmt::Debug for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A real three-digit HTTP status only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpStatus(u16);
impl HttpStatus {
    /// Reject values outside the HTTP status range.
    pub fn new(status: u16) -> Option<Self> {
        (100..=599).contains(&status).then_some(Self(status))
    }
    /// Numeric status code.
    pub fn code(self) -> u16 {
        self.0
    }
}
impl fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

labels! {
    /// Recognized media types; parameters never cross this boundary.
    pub enum ContentType {
        /// JSON.
        Json => "application/json",
        /// Problem JSON.
        ProblemJson => "application/problem+json",
        /// UTF text, without a charset parameter.
        Text => "text/plain",
        /// Binary.
        Binary => "application/octet-stream",
        /// Multipart, without its boundary.
        Multipart => "multipart/form-data"
    }
}
labels! {
    /// Single recognized content coding.
    pub enum ContentEncoding {
        /// No compression.
        Identity => "identity",
        /// Gzip.
        Gzip => "gzip",
        /// Deflate.
        Deflate => "deflate"
    }
}

/// A fixed omission marker distinguishes unsafe metadata from absent metadata.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Metadata<T> {
    /// Header absent.
    #[default]
    Absent,
    /// Unsupported, invalid, or ambiguous header.
    Omitted,
    /// Validated metadata.
    Value(T),
}
impl<T: fmt::Display> fmt::Display for Metadata<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => Ok(()),
            Self::Omitted => f.write_str("<omitted>"),
            Self::Value(value) => fmt::Display::fmt(value, f),
        }
    }
}

/// Exactly seven permitted header slots. No header names or values are strings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SafeHeaders {
    content_type: Metadata<ContentType>,
    content_length: Metadata<u64>,
    content_encoding: Metadata<ContentEncoding>,
    retry_after: Metadata<DiagnosticDuration>,
    request_id: Metadata<RequestId>,
    x_ms_request_id: Metadata<RequestId>,
    location: Metadata<SafeUrl>,
}
impl SafeHeaders {
    /// Construct only from already parsed, bounded metadata.
    pub fn new(
        content_type: Metadata<ContentType>,
        content_length: Metadata<u64>,
        content_encoding: Metadata<ContentEncoding>,
        retry_after: Metadata<DiagnosticDuration>,
        request_id: Metadata<RequestId>,
        x_ms_request_id: Metadata<RequestId>,
        location: Metadata<SafeUrl>,
    ) -> Self {
        Self {
            content_type,
            content_length,
            content_encoding,
            retry_after,
            request_id,
            x_ms_request_id,
            location,
        }
    }
    /// Parsed media type.
    pub fn content_type(self) -> Metadata<ContentType> {
        self.content_type
    }
    /// Parsed content length.
    pub fn content_length(self) -> Metadata<u64> {
        self.content_length
    }
    /// Parsed coding.
    pub fn content_encoding(self) -> Metadata<ContentEncoding> {
        self.content_encoding
    }
    /// Parsed not-before delay.
    pub fn retry_after(self) -> Metadata<DiagnosticDuration> {
        self.retry_after
    }
    /// Canonical requestId.
    pub fn request_id(self) -> Metadata<RequestId> {
        self.request_id
    }
    /// Canonical x-ms-request-id.
    pub fn x_ms_request_id(self) -> Metadata<RequestId> {
        self.x_ms_request_id
    }
    /// Validated and templated Location.
    pub fn location(self) -> Metadata<SafeUrl> {
        self.location
    }
}

labels! {
    /// Recognized polling protocols.
    pub enum PollingStrategy {
        /// Fabric operation.
        Fabric => "Fabric operation",
        /// Fabric job.
        FabricJob => "Fabric job",
        /// ARM status document.
        ArmStatus => "ARM status",
        /// ARM Location status.
        ArmLocation => "ARM Location"
    }
}
labels! {
    /// Why the selected delay was used.
    pub enum DelaySource {
        /// Fixed client policy.
        Client => "client schedule",
        /// Explicit caller interval.
        Configured => "configured interval",
        /// Validated server hint.
        Server => "server Retry-After",
        /// Invalid server hint was ignored.
        InvalidServerHint => "invalid Retry-After; client schedule"
    }
}
labels! {
    /// Work following a polling wait.
    pub enum PollPurpose {
        /// Status check.
        Status => "status check",
        /// Result retrieval.
        Result => "result retrieval"
    }
}
labels! {
    /// Result-selection decisions without exposing a handle or body.
    pub enum ResultAction {
        /// Status itself is the result.
        StatusResponse => "returning terminal status",
        /// ARM returns its initial representation.
        InitialResponse => "returning initial response",
        /// Fetch advertised result.
        Fetch => "retrieving result",
        /// Fetched result is ready.
        Retrieved => "result retrieved",
        /// Service explicitly has no separate result.
        NoResult => "operation has no separate result"
    }
}
labels! {
    /// Known backend terminal outcomes.
    pub enum OperationOutcome {
        /// Backend succeeded.
        Succeeded => "succeeded",
        /// Backend failed.
        Failed => "failed",
        /// Backend was cancelled.
        Cancelled => "cancelled"
    }
}
labels! {
    /// Fixed reasons for retrying a safe GET.
    pub enum RetryReason {
        /// Transient HTTP status.
        HttpStatus => "transient HTTP status",
        /// Uncertain transport error.
        Transport => "transport failure"
    }
}

/// Known remote state retained on a local reporting error, never a recovery handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticOutcome {
    /// No send began in this execution.
    BeforeSend,
    /// A send began without a known response.
    Uncertain,
    /// A response status was received.
    Received(HttpStatus),
    /// Earlier remote work was accepted before a later reporting error.
    Accepted,
    /// The backend terminal outcome is known.
    Terminal(OperationOutcome),
}

/// Safe, bounded event payloads. Numeric IDs are invocation-local, not service IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticEvent {
    /// CLI lifecycle, with no command arguments or identity information.
    Stage {
        /// Fixed stage.
        stage: DiagnosticStage,
        /// Lifecycle category.
        phase: DiagnosticPhase,
        /// Elapsed stage duration.
        duration: DiagnosticDuration,
    },
    /// Credential acquisition; never the token or provider error.
    Credential {
        /// Lifecycle category.
        phase: DiagnosticPhase,
        /// Elapsed acquisition duration.
        duration: DiagnosticDuration,
    },
    /// One actual HTTP attempt about to be sent.
    HttpSend {
        /// Invocation-local attempt ID.
        id: u64,
        /// One-based attempt within this execution.
        attempt: u32,
        /// Whitelisted method.
        method: DiagnosticMethod,
        /// Fixed URL labels.
        url: SafeUrl,
        /// Seven bounded header slots.
        headers: SafeHeaders,
    },
    /// A received response, including only approved metadata.
    HttpResponse {
        /// Matching invocation-local attempt ID.
        id: u64,
        /// Valid status, absent when malformed.
        status: Option<HttpStatus>,
        /// Send duration.
        duration: DiagnosticDuration,
        /// Seven bounded header slots.
        headers: SafeHeaders,
    },
    /// A failed attempt's safe category only.
    HttpFailure {
        /// Matching invocation-local attempt ID.
        id: u64,
        /// Received response-head status, when known despite a later failure.
        status: Option<HttpStatus>,
        /// Safe failure category.
        kind: ErrorKind,
        /// Send duration.
        duration: DiagnosticDuration,
    },
    /// A safe GET retry, never a mutation retry.
    Retry {
        /// Previous invocation-local attempt ID.
        id: u64,
        /// Next one-based attempt number.
        attempt: u32,
        /// Scheduled delay.
        delay: DiagnosticDuration,
        /// Fixed retry reason.
        reason: RetryReason,
        /// Source of the delay.
        source: DelaySource,
    },
    /// Validated operation strategy.
    Polling {
        /// Validated protocol choice.
        strategy: PollingStrategy,
    },
    /// A scheduled not-before wait.
    PollWait {
        /// Scheduled not-before delay.
        delay: DiagnosticDuration,
        /// Next action.
        purpose: PollPurpose,
        /// Source of the delay.
        source: DelaySource,
    },
    /// Result-selection choice.
    PollResult {
        /// Fixed result-selection choice.
        action: ResultAction,
    },
    /// Known terminal outcome.
    PollCompleted {
        /// Known backend terminal outcome.
        outcome: OperationOutcome,
    },
    /// Local reconciliation failure, including cancellation.
    PollFailed {
        /// Safe local failure category.
        kind: ErrorKind,
    },
}

/// Injected, acknowledged async diagnostics. Libraries never initialize a logger.
#[async_trait]
pub trait DiagnosticObserver: Send + Sync {
    /// Deliver one sanitized event or stop local work with a reporting failure.
    async fn observe(&self, event: DiagnosticEvent) -> Result<(), CoreError>;
}
