// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Guarded Azure Pipelines OIDC input and transport contracts.

use super::*;
use std::ffi::OsString;

const RUN_VARIABLES: [&str; 6] = [
    "SYSTEM_COLLECTIONURI",
    "SYSTEM_TEAMPROJECTID",
    "SYSTEM_HOSTTYPE",
    "SYSTEM_PLANID",
    "SYSTEM_JOBID",
    "SYSTEM_OIDCREQUESTURI",
];
const AUTHORIZATION: &str = "authorization";
const FEDAUTH: &str = "x-tfs-fedauthredirect";

/// Explicit Azure Pipelines run, service connection and system-token snapshot.
///
/// The official provider reads `SYSTEM_OIDCREQUESTURI` internally. Construction,
/// cache hits and outgoing requests therefore revalidate the running process
/// against this caller-captured run. Secrets never form identity bindings.
#[derive(Clone)]
pub struct PipelinesConfig {
    run: [String; 6],
    connection: String,
    system_token: Secret,
    request_url: Url,
}

impl fmt::Debug for PipelinesConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipelinesConfig").finish_non_exhaustive()
    }
}

impl PipelinesConfig {
    /// Captures an explicit running-pipeline contract without network I/O.
    ///
    /// `oidc_uri` must exactly match `SYSTEM_OIDCREQUESTURI` and the endpoint
    /// independently assembled from the selected collection, project, hub,
    /// plan and job. Only public `https://dev.azure.com/{organization}/`
    /// collections are supported. The system token is a secret caller input,
    /// not an access token for Fabric and not an environment-discovery trigger.
    ///
    /// # Errors
    /// Rejects missing, noncanonical or inconsistent inputs without echoing them.
    pub fn from_inputs(
        service_connection: &str,
        oidc_uri: &str,
        system_token: Secret,
        read: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, HeadlessError> {
        if !is_guid(service_connection) || !valid_header(system_token.secret()) {
            return Err(HeadlessError::Configuration);
        }
        let run = RUN_VARIABLES
            .map(|name| {
                read(name)
                    .and_then(|value| value.into_string().ok())
                    .ok_or(HeadlessError::Configuration)
            })
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let run: [String; 6] = run.try_into().map_err(|_| HeadlessError::Configuration)?;
        let collection = Url::parse(&run[0]).map_err(|_| HeadlessError::Configuration)?;
        let organization = collection
            .path()
            .strip_prefix('/')
            .and_then(|path| path.strip_suffix('/'));
        if collection.scheme() != "https"
            || collection.host_str() != Some("dev.azure.com")
            || collection.port().is_some()
            || !collection.username().is_empty()
            || collection.password().is_some()
            || collection.query().is_some()
            || collection.fragment().is_some()
            || collection.as_str() != run[0]
            || organization.is_none_or(|value| {
                value.is_empty()
                    || value.len() > 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
            || !is_guid(&run[1])
            || !matches!(run[2].as_str(), "build" | "rm" | "checks")
            || !is_guid(&run[3])
            || !is_guid(&run[4])
        {
            return Err(HeadlessError::Configuration);
        }
        let expected = format!(
            "{}{}/_apis/distributedtask/hubs/{}/plans/{}/jobs/{}/oidctoken",
            run[0], run[1], run[2], run[3], run[4]
        );
        if run[5] != expected || oidc_uri != expected {
            return Err(HeadlessError::Configuration);
        }
        let mut request_url = Url::parse(&expected).map_err(|_| HeadlessError::Configuration)?;
        request_url
            .query_pairs_mut()
            .append_pair("api-version", "7.1")
            .append_pair("serviceConnectionId", service_connection);
        Ok(Self {
            run,
            connection: service_connection.into(),
            system_token,
            request_url,
        })
    }

    /// Non-secret provider/connection/run binding for an identity-isolated batch.
    ///
    /// This is caller selection metadata, not proof of access-token claims.
    pub fn binding(&self) -> String {
        format!("azure-pipelines:{}:{}", self.connection, self.run[5])
    }

    pub(super) fn validate_environment(&self) -> Result<(), HeadlessError> {
        self.validate_with(|name| std::env::var_os(name))
    }

    fn validate_with(&self, read: impl Fn(&str) -> Option<OsString>) -> Result<(), HeadlessError> {
        for (name, expected) in RUN_VARIABLES.iter().zip(&self.run) {
            if read(name).as_deref() != Some(std::ffi::OsStr::new(expected)) {
                return Err(HeadlessError::PipelineContract);
            }
        }
        Ok(())
    }

    pub(super) fn provider(
        &self,
        identity: ServiceIdentity,
        options: ClientOptions,
    ) -> Result<Arc<dyn TokenCredential>, HeadlessError> {
        self.validate_environment()?;
        let credential = azure_identity::AzurePipelinesCredential::new(
            identity.tenant().into(),
            identity.client().into(),
            &self.connection,
            self.system_token.clone(),
            Some(azure_identity::AzurePipelinesCredentialOptions {
                credential_options: ClientAssertionCredentialOptions {
                    client_options: options,
                },
            }),
        )
        .map_err(|_| HeadlessError::Configuration)?;
        self.validate_environment()?;
        Ok(credential)
    }
}

pub(super) struct PipelinesHttp {
    pub(super) token_http: CredentialHttp,
    pub(super) config: PipelinesConfig,
}

impl fmt::Debug for PipelinesHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipelinesHttp").finish_non_exhaustive()
    }
}

impl PipelinesHttp {
    fn validate_request(&self, request: &Request) -> Result<bool, HeadlessError> {
        if request.url() == &self.config.request_url {
            let authorization = format!("Bearer {}", self.config.system_token.secret());
            if request.method() != Method::Post
                || request.headers().get_str(&AUTHORIZATION.into()).ok()
                    != Some(authorization.as_str())
                || request.headers().get_str(&FEDAUTH.into()).ok() != Some("Suppress")
                || request.headers().get_str(&"content-length".into()).ok() != Some("0")
                || !azure_core::Bytes::from(request.body()).is_empty()
            {
                return Err(HeadlessError::PipelineContract);
            }
            return Ok(true);
        }
        if request.url() != &self.token_http.endpoint
            || request.headers().get_str(&AUTHORIZATION.into()).is_ok()
            || request.headers().get_str(&FEDAUTH.into()).is_ok()
        {
            return Err(HeadlessError::PipelineContract);
        }
        Ok(false)
    }

    async fn execute_with(
        &self,
        request: &Request,
        read: impl Fn(&str) -> Option<OsString>,
    ) -> azure_core::Result<AsyncRawResponse> {
        self.config.validate_with(read).map_err(credential_error)?;
        if !self.validate_request(request).map_err(credential_error)? {
            let response = self.token_http.execute_request(request).await?;
            let (status, _, body) = response.deconstruct();
            return Ok(AsyncRawResponse::new(
                status,
                azure_core::http::headers::Headers::new(),
                Box::pin(body),
            ));
        }
        // The injected transport must not follow redirects or retry requests.
        // Never hand OIDC error bodies/headers to the official error formatter.
        let response = self
            .token_http
            .inner
            .execute_request(request)
            .await
            .map_err(|_| credential_error(HeadlessError::Unavailable))?;
        let status: u16 = response.status().into();
        if status != 200 {
            return Err(credential_error(if status == 429 || status >= 500 {
                HeadlessError::Unavailable
            } else {
                HeadlessError::Rejected
            }));
        }
        let (_, _, body) = response.deconstruct();
        let mut bytes = vec![0; ASSERTION_LIMIT as usize];
        let length = body
            .collect_into(&mut bytes)
            .await
            .map_err(|_| credential_error(HeadlessError::TokenMetadata))?;
        bytes.truncate(length);
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| credential_error(HeadlessError::TokenMetadata))?;
        if value
            .get("oidcToken")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| {
                value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_graphic())
            })
        {
            return Err(credential_error(HeadlessError::TokenMetadata));
        }
        Ok(AsyncRawResponse::from_bytes(
            azure_core::http::StatusCode::Ok,
            azure_core::http::headers::Headers::new(),
            bytes,
        ))
    }
}

#[async_trait]
impl HttpClient for PipelinesHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.execute_with(request, |name| std::env::var_os(name))
            .await
    }
}

#[cfg(test)]
#[path = "pipelines_tests.rs"]
mod tests;
