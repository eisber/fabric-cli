// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated, caller-supplied service endpoints.

use crate::{Audience, CoreError, ErrorKind};
use url::Url;

/// A service audience and its validated HTTPS base URL.
#[derive(Clone, Debug)]
pub struct Endpoint {
    audience: Audience,
    base_url: Url,
}

impl Endpoint {
    /// Validate a hostname using Python's endpoint hostname patterns.
    ///
    /// As in Python, any suffix beginning with `/` is discarded. URL parsing
    /// additionally rejects non-ASCII and noncanonical hostname spellings.
    pub fn from_hostname(audience: Audience, value: &str) -> Result<Self, CoreError> {
        let host = value.split('/').next().unwrap_or_default();
        let invalid = || {
            CoreError::new(
                ErrorKind::InvalidInput,
                format!(
                    "Invalid hostname in {}",
                    Self::environment_variable(audience)
                ),
            )
        };
        if !valid_hostname(host) {
            return Err(invalid());
        }
        let path = match audience {
            Audience::Fabric => "/v1",
            Audience::Storage | Audience::Azure => "",
            Audience::PowerBi => "/v1.0/myorg",
        };
        let base_url = Url::parse(&format!("https://{host}{path}")).map_err(|_| invalid())?;
        if base_url.host_str() != Some(host)
            || base_url.port().is_some()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
        {
            return Err(invalid());
        }
        Ok(Self { audience, base_url })
    }

    /// The environment variable for the selected audience.
    pub fn environment_variable(audience: Audience) -> &'static str {
        match audience {
            Audience::Fabric => "FABRIC_API_ENDPOINT",
            Audience::Storage => "ONELAKE_API_ENDPOINT",
            Audience::Azure => "AZURE_API_ENDPOINT",
            Audience::PowerBi => "POWER_BI_API_ENDPOINT",
        }
    }

    /// Construct the fixed public endpoint for an audience.
    pub fn public(audience: Audience) -> Self {
        Self {
            audience,
            base_url: Url::parse(audience.base_url()).expect("fixed audience URL"),
        }
    }

    /// The audience determines the credential scope independently of the hostname.
    pub fn audience(&self) -> Audience {
        self.audience
    }

    /// The validated HTTPS service base URL.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Validate an initial request against this endpoint before authentication.
    pub fn validate_url(&self, url: &Url) -> Result<(), CoreError> {
        crate::validate_url_for_host(url, self.base_url.host_str().expect("validated host"))
    }
}

fn valid_hostname(host: &str) -> bool {
    fn label(value: &str) -> bool {
        !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    }
    let parts: Vec<_> = host.split('.').collect();
    let without_optional_prefix = |suffix: &[&str]| {
        parts.as_slice() == suffix
            || (parts.len() == suffix.len() + 1 && label(parts[0]) && &parts[1..] == suffix)
    };
    if without_optional_prefix(&["fabric", "microsoft", "com"])
        || without_optional_prefix(&["dfs", "fabric", "microsoft", "com"])
        || without_optional_prefix(&["powerbi", "com"])
    {
        return true;
    }
    let management = if parts.len() == 4 && label(parts[0]) {
        &parts[1..]
    } else {
        parts.as_slice()
    };
    matches!(management, ["management", first, second] if label(first) && label(second))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_patterns_and_paths_match_endpoint_rules() {
        for host in [
            "api.fabric.microsoft.com",
            "msitapi.fabric.microsoft.com",
            "onelake.dfs.fabric.microsoft.com",
            "api.powerbi.com",
            "management.azure.com",
            "prefix.management.example.com",
        ] {
            assert!(
                Endpoint::from_hostname(Audience::Fabric, host).is_ok(),
                "{host}"
            );
        }
        let endpoint = Endpoint::from_hostname(Audience::PowerBi, "api.powerbi.com/ignored")
            .expect("valid host");
        assert_eq!(
            endpoint.base_url().as_str(),
            "https://api.powerbi.com/v1.0/myorg"
        );
    }

    #[test]
    fn rejects_unsafe_and_nonmatching_values_without_echoing_them() {
        for host in [
            "",
            "https://api.fabric.microsoft.com",
            "api.fabric.microsoft.com:443",
            "user@api.fabric.microsoft.com",
            "api.fabric.microsoft.com.evil.test",
            "api.fabric.microsoft.com?secret",
            "api.fabric.microsoft.com\n",
        ] {
            let error = Endpoint::from_hostname(Audience::Fabric, host).unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidInput);
            assert_eq!(error.message, "Invalid hostname in FABRIC_API_ENDPOINT");
        }
    }
}
