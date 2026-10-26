// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::JobError;
use std::fmt;

/// Explicit caller-owned, job-scoped request headers.
///
/// Only Python's `x-ms-fabric-skill` is supported. This cannot override
/// authentication, host, audience, content type or other transport policy.
/// No value is read from the environment or global CLI context.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct JobRequestHeaders {
    values: Vec<(String, String)>,
}

impl JobRequestHeaders {
    /// Select the exact source skill value; absent or empty values emit no header.
    ///
    /// Rejects control characters, a leading ASCII space and values beyond the
    /// shared executor's 16 KiB header budget before credentials or I/O.
    pub fn with_skill(skill: Option<&str>) -> Result<Self, JobError> {
        let Some(skill) = skill.filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        const NAME: &str = "x-ms-fabric-skill";
        if skill.chars().any(char::is_control)
            || skill.starts_with(' ')
            || skill.len() > 16 * 1024 - NAME.len() - 4
        {
            return Err(JobError::InvalidRequestHeaders);
        }
        Ok(Self {
            values: vec![(NAME.into(), skill.into())],
        })
    }

    /// Borrow validated header fields; skill data must not be logged automatically.
    pub fn as_slice(&self) -> &[(String, String)] {
        &self.values
    }

    /// Whether the source selected any header.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl fmt::Debug for JobRequestHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRequestHeaders")
            .field("has_skill", &!self.is_empty())
            .finish()
    }
}
