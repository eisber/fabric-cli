// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit service identity and immutable, validated Microsoft endpoint bindings.
//!
//! The hostname families follow Python's `fab_hostname_validator.py` at
//! `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. Unlike its environment-variable
//! adapter, this boundary accepts only complete lowercase ASCII DNS hostnames,
//! checks domain boundaries, and restricts ARM overrides to public-cloud Azure.
//! Selecting a hostname never changes a service's audience or API root.

use crate::Audience;
use azure_core::http::Url;
use thiserror::Error;

/// A service identity independent of the hostname selected for its requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceOrigin {
    /// The Fabric REST API.
    Fabric,
    /// OneLake storage.
    OneLake,
    /// Azure Resource Manager.
    Arm,
    /// The Power BI REST API, using the Fabric token audience.
    PowerBi,
}

impl ServiceOrigin {
    /// Returns the fixed public-cloud token audience for this service.
    pub const fn audience(self) -> Audience {
        match self {
            Self::Fabric | Self::PowerBi => Audience::Fabric,
            Self::OneLake => Audience::OneLake,
            Self::Arm => Audience::Arm,
        }
    }

    /// Returns this service's default public-cloud hostname.
    pub const fn host(self) -> &'static str {
        match self {
            Self::Fabric => "api.fabric.microsoft.com",
            Self::OneLake => "onelake.dfs.fabric.microsoft.com",
            Self::Arm => "management.azure.com",
            Self::PowerBi => "api.powerbi.com",
        }
    }

    /// Returns the service's API root, without adding an operation API version.
    pub const fn root_path(self) -> &'static str {
        match self {
            Self::Fabric => "/v1",
            Self::PowerBi => "/v1.0/myorg",
            Self::OneLake | Self::Arm => "",
        }
    }
}

/// A rejected endpoint or link, without echoing caller-supplied URI contents.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the endpoint or request URL is invalid")]
pub struct EndpointError;

/// An immutable service identity and an explicitly selected HTTPS hostname.
///
/// Host overrides may use any permitted Microsoft hostname family. They do not
/// infer a different service, audience, API root, or authentication provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointBinding {
    origin: ServiceOrigin,
    host: String,
    base_url: Url,
}

impl EndpointBinding {
    /// Selects the service's built-in public-cloud endpoint without ambient input.
    pub fn public(origin: ServiceOrigin) -> Self {
        Self::with_host(origin, origin.host()).expect("built-in public-cloud hostnames are valid")
    }

    /// Binds a service to a complete lowercase ASCII Microsoft DNS hostname.
    ///
    /// Permitted suffixes are `fabric.microsoft.com`, `powerbi.com`, and
    /// `management.azure.com`, including their subdomains. The Fabric suffix
    /// includes `dfs.fabric.microsoft.com` and `dailyapi.fabric.microsoft.com`.
    /// Schemes, ports, paths, IP addresses, Unicode, trailing dots, escapes, and
    /// malformed DNS labels are not accepted.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError`] if the hostname fails these checks.
    pub fn with_host(origin: ServiceOrigin, host: &str) -> Result<Self, EndpointError> {
        validate_host(host)?;
        let base_url = Url::parse(&format!("https://{host}/")).map_err(|_| EndpointError)?;
        if base_url.host_str() != Some(host) {
            return Err(EndpointError);
        }
        Ok(Self {
            origin,
            host: host.to_owned(),
            base_url,
        })
    }

    /// Returns the explicitly selected service identity.
    pub const fn origin(&self) -> ServiceOrigin {
        self.origin
    }

    /// Returns the service audience, never an audience inferred from the host.
    pub const fn audience(&self) -> Audience {
        self.origin.audience()
    }

    /// Returns the validated hostname.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the stored HTTPS origin URL with a root path and no query.
    pub fn base_url(&self) -> Url {
        self.base_url.clone()
    }

    /// Returns the selected service's API root, without an operation API version.
    pub const fn api_path(&self) -> &'static str {
        self.origin.root_path()
    }

    /// Resolves a root-relative path and optional query against the selected host.
    ///
    /// The path must start with exactly one `/`. Encoded item segments and query
    /// values are preserved, but URL parsing must not normalize the path.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError`] for an authority, backslash, control character,
    /// fragment, dot-segment normalization, or any change to the selected origin.
    pub fn url(&self, path: &str) -> Result<Url, EndpointError> {
        validate_relative_path(path)?;
        let url = self.base_url.join(path).map_err(|_| EndpointError)?;
        self.validate_url(&url)?;
        let expected_path = path.split_once('?').map_or(path, |(path, _)| path);
        if url.path() != expected_path {
            return Err(EndpointError);
        }
        Ok(url)
    }

    /// Checks a parsed URL before a request can use a credential.
    ///
    /// Queries are allowed; HTTPS, the exact selected host, port 443, and no
    /// user information or fragment are required. Use [`Self::validate_link`]
    /// for unparsed links, before a URL parser can discard unsafe raw syntax.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError`] for an origin, credential, or fragment mismatch.
    pub fn validate_url(&self, url: &Url) -> Result<(), EndpointError> {
        if url.scheme() != "https"
            || url.host_str() != Some(self.host())
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(EndpointError);
        }
        Ok(())
    }

    /// Validates an absolute HTTPS link to the exact expected path on this host.
    ///
    /// Raw authority syntax is checked before parsing to reject escaped hosts,
    /// user information, controls, backslashes, and normalization tricks. The
    /// only optional port is literal `443`. Queries and fragments are forbidden.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError`] for unsafe URI syntax, a different origin or
    /// path, or a query or fragment, including an empty query or fragment.
    pub fn validate_link(&self, uri: &str, path: &str) -> Result<Url, EndpointError> {
        if uri
            .chars()
            .any(|character| character.is_control() || character == '\\')
            || uri.contains(['?', '#'])
            || path.contains('?')
        {
            return Err(EndpointError);
        }
        self.url(path)?;
        let rest = uri.strip_prefix("https://").ok_or(EndpointError)?;
        let (authority, raw_path) = rest.split_once('/').ok_or(EndpointError)?;
        let authority_host = authority.strip_suffix(":443").unwrap_or(authority);
        if authority_host != self.host() {
            return Err(EndpointError);
        }
        if raw_path != path.strip_prefix('/').ok_or(EndpointError)? {
            return Err(EndpointError);
        }
        let url = Url::parse(uri).map_err(|_| EndpointError)?;
        self.validate_url(&url)?;
        if url.path() != path || url.query().is_some() {
            return Err(EndpointError);
        }
        Ok(url)
    }
}

fn validate_host(host: &str) -> Result<(), EndpointError> {
    if host.is_empty() || host.len() > 253 {
        return Err(EndpointError);
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(EndpointError);
        }
    }
    if ![
        "fabric.microsoft.com",
        "powerbi.com",
        "management.azure.com",
    ]
    .iter()
    .any(|suffix| {
        host == *suffix
            || host
                .strip_suffix(*suffix)
                .is_some_and(|prefix| prefix.ends_with('.'))
    }) {
        return Err(EndpointError);
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), EndpointError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('#')
        || path
            .chars()
            .any(|character| character.is_control() || character == '\\')
    {
        return Err(EndpointError);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_defaults_have_fixed_audiences_and_roots() {
        for (origin, host, audience, root) in [
            (
                ServiceOrigin::Fabric,
                "api.fabric.microsoft.com",
                Audience::Fabric,
                "/v1",
            ),
            (
                ServiceOrigin::OneLake,
                "onelake.dfs.fabric.microsoft.com",
                Audience::OneLake,
                "",
            ),
            (
                ServiceOrigin::Arm,
                "management.azure.com",
                Audience::Arm,
                "",
            ),
            (
                ServiceOrigin::PowerBi,
                "api.powerbi.com",
                Audience::Fabric,
                "/v1.0/myorg",
            ),
        ] {
            let binding = EndpointBinding::public(origin);
            assert_eq!(binding.origin(), origin);
            assert_eq!(origin.host(), host);
            assert_eq!(binding.host(), host);
            assert_eq!(binding.audience(), audience);
            assert_eq!(binding.api_path(), root);
            assert_eq!(binding.base_url().as_str(), format!("https://{host}/"));
        }
    }

    #[test]
    fn daily_and_cross_family_overrides_do_not_infer_the_service() {
        for origin in [
            ServiceOrigin::Fabric,
            ServiceOrigin::OneLake,
            ServiceOrigin::Arm,
            ServiceOrigin::PowerBi,
        ] {
            let daily =
                EndpointBinding::with_host(origin, "dailyapi.fabric.microsoft.com").unwrap();
            assert_eq!(daily.audience().scope(), origin.audience().scope());
            assert_eq!(daily.api_path(), origin.root_path());
            assert_eq!(daily.origin(), origin);
        }
        for host in [
            "fabric.microsoft.com",
            "dfs.fabric.microsoft.com",
            "onelake.dfs.fabric.microsoft.com",
            "daily.api.fabric.microsoft.com",
            "powerbi.com",
            "api.powerbi.com",
            "management.azure.com",
            "test.management.azure.com",
        ] {
            assert!(EndpointBinding::with_host(ServiceOrigin::Fabric, host).is_ok());
        }
    }

    #[test]
    fn rejects_non_hostname_syntax_and_suffix_confusion() {
        for host in [
            "",
            ".",
            "api.fabric.microsoft.com.",
            ".api.fabric.microsoft.com",
            "api..fabric.microsoft.com",
            "https://api.fabric.microsoft.com",
            "api.fabric.microsoft.com:443",
            "api.fabric.microsoft.com/path",
            "api.fabric.microsoft.com?query",
            "api.fabric.microsoft.com#fragment",
            "user@api.fabric.microsoft.com",
            "api.fabric.microsoft.com\\path",
            "api.fabric.microsoft.com\n",
            " api.fabric.microsoft.com",
            "api.fabric.microsoft.com ",
            "api\t.fabric.microsoft.com",
            "API.fabric.microsoft.com",
            "apı.fabric.microsoft.com",
            "api.fabric.microsoft.com\u{3002}",
            "api%2efabric.microsoft.com",
            "127.0.0.1",
            "[::1]",
            "0x7f000001",
            "evilfabric.microsoft.com",
            "evilpowerbi.com",
            "evilmanagement.azure.com",
            "api.fabric.microsoft.com.example.com",
            "management.example.com",
            "-api.fabric.microsoft.com",
            "api-.fabric.microsoft.com",
            "api_name.fabric.microsoft.com",
        ] {
            assert_eq!(
                EndpointBinding::with_host(ServiceOrigin::Fabric, host),
                Err(EndpointError),
                "{host:?}"
            );
        }
        let long_label = format!("{}.fabric.microsoft.com", "a".repeat(64));
        assert!(EndpointBinding::with_host(ServiceOrigin::Fabric, &long_label).is_err());
        let long_host = format!("{}.fabric.microsoft.com", vec!["a".repeat(63); 4].join("."));
        assert!(EndpointBinding::with_host(ServiceOrigin::Fabric, &long_host).is_err());
    }

    #[test]
    fn path_construction_preserves_encoded_items_and_queries() {
        let binding = EndpointBinding::public(ServiceOrigin::Fabric);
        let path = "/v1/items/a%2Fb%20c?continuationToken=opaque%2B%2F%3D";
        assert_eq!(
            binding.url(path).unwrap().as_str(),
            format!("https://api.fabric.microsoft.com{path}")
        );
        for path in [
            "",
            "v1/items",
            "//api.powerbi.com/v1/items",
            "/\\api.powerbi.com/v1/items",
            "/v1/../items",
            "/v1/./items",
            "/v1/%2e%2e/items",
            "/v1/.%2E/items",
            "/v1/%2E/items",
            "/v1/items#fragment",
            "/v1/it\nems",
            "/v1/items?value=\r",
            "/v1/items?value=\u{85}",
            "/v1/items?value=\\",
            "/v1/a b",
        ] {
            assert_eq!(binding.url(path), Err(EndpointError), "{path:?}");
        }
    }

    #[test]
    fn workspace_item_paths_preserve_opaque_segments_but_reject_dot_segments() {
        let binding = EndpointBinding::public(ServiceOrigin::Fabric);
        for path in [
            "/v1/workspaces/ws%2Fopaque%3F%23%25/items/item%20name%2B%3D%40%3A",
            "/v1/workspaces/ws%2fopaque/items/%E2%98%83",
            "/v1/workspaces/ws/items/part%2ejson",
            "/v1/workspaces/ws/items/%252e%252e",
        ] {
            let uri = format!("https://api.fabric.microsoft.com{path}");
            assert_eq!(binding.url(path).unwrap().as_str(), uri);
            assert_eq!(binding.validate_link(&uri, path).unwrap().as_str(), uri);
        }
        for path in [
            "/v1/workspaces/../items/item",
            "/v1/workspaces/%2e/items/item",
            "/v1/workspaces/%2E./items/item",
            "/v1/workspaces/.%2e/items/item",
            "/v1/workspaces/%2e%2E/items/item",
            "/v1/workspaces/ws/items/..",
            "/v1/workspaces/ws/items/%2e%2e",
        ] {
            let uri = format!("https://api.fabric.microsoft.com{path}");
            assert_eq!(binding.url(path), Err(EndpointError), "{path:?}");
            assert_eq!(
                binding.validate_link(&uri, path),
                Err(EndpointError),
                "{uri:?}"
            );
        }
    }

    #[test]
    fn parsed_urls_are_bound_to_exact_https_host_and_port() {
        let binding = EndpointBinding::public(ServiceOrigin::Fabric);
        assert!(
            binding
                .validate_url(&Url::parse("https://api.fabric.microsoft.com:443/v1?q=x").unwrap())
                .is_ok()
        );
        for uri in [
            "http://api.fabric.microsoft.com/v1",
            "https://api.fabric.microsoft.com:444/v1",
            "https://api.powerbi.com/v1",
            "https://dailyapi.fabric.microsoft.com/v1",
            "https://api.fabric.microsoft.com./v1",
            "https://user@api.fabric.microsoft.com/v1",
            "https://:password@api.fabric.microsoft.com/v1",
            "https://api.fabric.microsoft.com/v1#",
        ] {
            assert_eq!(
                binding.validate_url(&Url::parse(uri).unwrap()),
                Err(EndpointError),
                "{uri:?}"
            );
        }
    }

    #[test]
    fn raw_links_reject_parsing_tricks_and_require_exact_queryless_path() {
        let binding = EndpointBinding::public(ServiceOrigin::Fabric);
        for uri in [
            "https://api.fabric.microsoft.com/v1/items/a%2Fb",
            "https://api.fabric.microsoft.com:443/v1/items/a%2Fb",
        ] {
            assert!(binding.validate_link(uri, "/v1/items/a%2Fb").is_ok());
        }
        for uri in [
            "//api.fabric.microsoft.com/v1/items",
            "http://api.fabric.microsoft.com/v1/items",
            "https://api.fabric.microsoft.com/v1/items?",
            "https://api.fabric.microsoft.com/v1/items?token=x",
            "https://api.fabric.microsoft.com/v1/items#",
            "https://api.fabric.microsoft.com/v1/other/../items",
            "https://api.fabric.microsoft.com/v1/%2e/items",
            "https://%61pi.fabric.microsoft.com/v1/items",
            "https://api.fabric.microsoft.com./v1/items",
            "https://api.fabric.microsof\u{0074}\n.com/v1/items",
            "https://api.fabric.microsoft.com\\@evil.example/v1/items",
            "https://@api.fabric.microsoft.com/v1/items",
            "https://api.fabric.microsoft.com:0443/v1/items",
            "https://api.fabric.microsoft.com:443:443/v1/items",
            "https://api.fabric.microsoft.com/v1/items/",
            "https://dailyapi.fabric.microsoft.com/v1/items",
        ] {
            assert_eq!(
                binding.validate_link(uri, "/v1/items"),
                Err(EndpointError),
                "{uri:?}"
            );
        }
    }
}
