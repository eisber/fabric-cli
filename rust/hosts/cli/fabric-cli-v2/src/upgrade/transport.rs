// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use azure_core::http::{HttpClient, Method, Request, Url};
use fabric_cli_update::{Control, Error, HttpRequest, HttpResponse, HttpTransport, ResponseSink};
use fabric_sdk_transport::{NativeHttpClient, ProxyPolicy, TransportOptions, TrustPolicy};
use futures_util::StreamExt;

use super::native::{HostControl, failure};
use crate::CliError;

/// Only translates the existing engine's anonymous requests to native HTTP.
///
/// Origin, candidate, redirect, signature and retry policy remain in the engine.
/// The session drives the existing Tokio/native HTTP host composition; this
/// adapter creates no alternative TLS stack, credential, policy store,
/// automatic updater or cancellation poller.
pub(super) struct AnonymousTransport {
    runtime: tokio::runtime::Handle,
    http: NativeHttpClient,
    control: Arc<HostControl>,
}

fn anonymous_request(pending: &HttpRequest) -> Result<Request, Error> {
    let url = Url::parse(&pending.url).map_err(|_| Error::Origin)?;
    let mut request = Request::new(url, Method::Get);
    request.insert_header("accept", pending.accept);
    request.insert_header(
        "user-agent",
        concat!("fabric-cli-v2-update/", env!("CARGO_PKG_VERSION")),
    );
    request.insert_header("x-github-api-version", pending.api_version);
    request.insert_header("accept-encoding", "identity");
    Ok(request)
}

impl AnonymousTransport {
    pub(super) fn new(
        runtime: tokio::runtime::Handle,
        control: Arc<HostControl>,
    ) -> Result<Self, CliError> {
        let http = NativeHttpClient::new(TransportOptions {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
            pool_idle_timeout: Duration::from_secs(10),
            max_idle_per_host: NonZeroUsize::new(1).ok_or_else(|| failure(Error::InvalidLimits))?,
            trust: TrustPolicy::Platform {
                additional_roots: Vec::new(),
            },
            proxy: ProxyPolicy::None,
        })
        .map_err(|_| failure(Error::Transport))?;
        Ok(Self {
            runtime,
            http,
            control,
        })
    }
}

impl HttpTransport for AnonymousTransport {
    fn get(
        &mut self,
        pending: &HttpRequest,
        sink: &mut dyn ResponseSink,
        control: &dyn Control,
    ) -> Result<HttpResponse, Error> {
        let now = control.snapshot();
        if now.cancelled {
            return Err(Error::Cancelled);
        }
        let remaining = pending
            .deadline_ms
            .checked_sub(now.monotonic_ms)
            .filter(|value| *value != 0)
            .ok_or(Error::Deadline)?;
        let request = anonymous_request(pending)?;
        self.runtime.block_on(async {
            let operation = async {
                let response = self
                    .http
                    .execute_request(&request)
                    .await
                    .map_err(|_| Error::Transport)?;
                let status = u16::from(response.status());
                if response
                    .headers()
                    .get_str(&"content-encoding".into())
                    .is_ok_and(|value| !value.eq_ignore_ascii_case("identity"))
                {
                    return Err(Error::Response);
                }

                if let Ok(value) = response.headers().get_str(&"content-length".into()) {
                    let length = value.parse::<u64>().map_err(|_| Error::Response)?;
                    if length > pending.max_bytes as u64 {
                        return Err(Error::Limit);
                    }
                }
                let location = response
                    .headers()
                    .get_str(&"location".into())
                    .ok()
                    .map(str::to_owned);
                if location.as_ref().is_some_and(|value| value.len() > 4096) {
                    return Err(Error::Origin);
                }
                if status == 200 {
                    let mut body = response.into_body();
                    let mut bytes = 0_usize;
                    while let Some(chunk) = body.next().await {
                        let chunk = chunk.map_err(|_| Error::Transport)?;
                        bytes = bytes.checked_add(chunk.len()).ok_or(Error::Limit)?;
                        if bytes > pending.max_bytes {
                            return Err(Error::Limit);
                        }
                        sink.write(&chunk)?;
                    }
                }
                Ok(HttpResponse { status, location })
            };
            tokio::select! {
                result = operation => result,
                _ = tokio::time::sleep(Duration::from_millis(remaining)) => Err(Error::Deadline),
                _ = self.control.cancellation.notified() => Err(Error::Cancelled),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_get_only_and_have_no_credentials_or_decompression() {
        let request = anonymous_request(&HttpRequest {
            url: "https://api.github.com/repos/microsoft/fabric-cli/releases?per_page=20"
                .to_owned(),
            deadline_ms: 30_000,
            max_bytes: 64 * 1024,
            accept: "application/vnd.github+json",
            api_version: "2022-11-28",
        })
        .unwrap();
        for forbidden in ["authorization", "cookie", "proxy-authorization"] {
            assert!(request.headers().get_str(&forbidden.into()).is_err());
        }
        assert_eq!(
            request
                .headers()
                .get_str(&"accept-encoding".into())
                .unwrap(),
            "identity"
        );
    }
}
