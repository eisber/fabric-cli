// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::fmt;

/// Sanitized construction and configuration failures, without provider details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    /// A socket or pool duration was zero or greater than 24 hours.
    InvalidTimeout,
    /// A root or pool count exceeded the documented construction bound.
    InvalidConfiguration,
    /// An additional root certificate was malformed or oversized.
    InvalidCertificate,
    /// A proxy was not an unauthenticated static HTTP origin.
    InvalidProxy,
    /// The platform trust verifier could not be constructed.
    PlatformTrust,
    /// The established HTTP library could not construct the client.
    ClientConstruction,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidTimeout => "transport duration must be nonzero and at most 24 hours",
            Self::InvalidConfiguration => "transport configuration exceeds its construction bounds",
            Self::InvalidCertificate => "invalid additional trust certificate",
            Self::InvalidProxy => "unsupported static HTTP proxy configuration",
            Self::PlatformTrust => "platform certificate trust could not be configured",
            Self::ClientConstruction => "native HTTP client could not be constructed",
        })
    }
}

impl std::error::Error for TransportError {}
