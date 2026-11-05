// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-pinned Notebook monitoring, log and resource requests.
//!
//! Public AAD and workload MWC endpoints are deliberately separate. These
//! builders never discover IDs, choose credentials, follow signed links, write
//! files, or retry/fallback. Execute through the caller's shared native executor.
//!
//! ID kinds are not interchangeable, even when their strings look alike:
//!
//! ```compile_fail
//! use fabric_sdk::notebook_observability::{ActivityId, ApplicationId, PublicApplication};
//! let activity = ActivityId::new("activity").unwrap();
//! let app = ApplicationId::new("app").unwrap();
//! let wrong_owner = PublicApplication::new(activity, app, None);
//! ```
//!
//! ```compile_fail
//! use fabric_sdk::notebook_observability::{
//!     ApplicationId, ApplicationLocation, LivyId, WorkloadApplication,
//! };
//! let livy = LivyId::new("livy").unwrap();
//! let app = ApplicationId::new("app").unwrap();
//! let wrong_owner = WorkloadApplication::new(livy, app, ApplicationLocation::Active);
//! ```
//!
//! ```compile_fail
//! use fabric_sdk::notebook_observability::{ContainerId, SparkJobId};
//! let job = SparkJobId::new("0").unwrap();
//! let wrong_container: ContainerId = job;
//! ```

mod catalog;
mod plans;
mod public;
mod resources;
mod workload;

pub use catalog::{OperationCatalogEntry, SOURCE_OPERATIONS, SourceFamily};
pub use plans::{
    PlanCoverage, PlanResponses, ProgressResponses, RequestPlan, SignedEventLogTicket,
    WorkloadProgressPlan,
};
pub use public::{PublicApplication, PublicMonitor, PublicView};
pub use resources::{
    EntryType, ResourceContent, ResourcePath, ResourceWriteAdmission, Resources,
};
pub use workload::{
    ApplicationLocation, ApplicationView, MetricsWindow, WorkloadApplication, WorkloadMonitor,
    WorkloadView,
};

use crate::runtime::{NativeEndpoint, NativeError, validate_segment};

macro_rules! identifier {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Eq, PartialEq)]
        pub struct $name(String);

        impl $name {
            /// Validate an opaque identifier without converting another ID kind.
            ///
            /// # Errors
            /// Rejects empty IDs, path separators, escapes and controls.
            pub fn new(value: impl Into<String>) -> Result<Self, NativeError> {
                let value = value.into();
                validate_segment(&value)?;
                Ok(Self(value))
            }

            /// Borrow the exact caller-supplied value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(<opaque>)"))
            }
        }
    };
}

identifier!(LivyId, "A public Livy-history session ID, never an activity ID.");
identifier!(ActivityId, "A workload activity ID, never a public Livy ID.");
identifier!(ApplicationId, "An exact Spark application ID.");
identifier!(AttemptId, "An explicit completed-application attempt ID.");
identifier!(SparkJobId, "A Spark job ID, not a job-group or activity ID.");
identifier!(JobGroupId, "A runtime job-group ID, not an authored cell ID.");
identifier!(ContainerId, "A YARN container ID, not an executor ordinal.");
identifier!(StageId, "An exact Spark stage ID.");
identifier!(SqlExecutionId, "A Spark SQL execution-history ID.");

/// A provider-native log file name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogFile {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl LogFile {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Explicit log byte window; no implicit polling or pagination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogWindow {
    /// Exact provider byte offset.
    pub offset: u64,
    /// Positive maximum bytes requested from the provider.
    pub size: u32,
}

impl LogWindow {
    pub(crate) fn query(self) -> Result<Vec<(&'static str, String)>, NativeError> {
        if self.size == 0 {
            return Err(NativeError::InvalidInput);
        }
        Ok(vec![("offset", self.offset.to_string()), ("size", self.size.to_string())])
    }
}

/// Native SparkUI lifecycle collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobType {
    /// Interactive sessions.
    Sessions,
    /// Batch jobs.
    Batches,
}

impl JobType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Batches => "batches",
        }
    }
}

pub(crate) fn require_prefix(endpoint: &NativeEndpoint, expected: &str) -> Result<(), NativeError> {
    if endpoint.base().path().trim_end_matches('/') != expected {
        return Err(NativeError::UnsupportedCapability);
    }
    Ok(())
}

pub(crate) fn validate_scope(scope: &[&str]) -> Result<(), NativeError> {
    for value in scope {
        validate_segment(value)?;
    }
    Ok(())
}

pub(crate) fn positive(value: u32) -> Result<String, NativeError> {
    if value == 0 {
        return Err(NativeError::InvalidInput);
    }
    Ok(value.to_string())
}
