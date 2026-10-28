// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{ProxyPolicy, TransportError};
use http::Uri;
use hyper_util::{
    client::legacy::connect::{HttpConnector, proxy::Tunnel},
    rt::TokioIo,
};
use std::{
    error::Error,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::TcpStream;
use tower_service::Service;

type BoxError = Box<dyn Error + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Connector {
    direct: HttpConnector,
    proxy: Option<Tunnel<HttpConnector>>,
    #[cfg(test)]
    pub(crate) test_address: Option<std::net::SocketAddr>,
}

impl Connector {
    pub(crate) fn new(proxy: ProxyPolicy) -> Result<Self, TransportError> {
        let mut direct = HttpConnector::new();
        direct.enforce_http(false);
        let proxy = match proxy {
            ProxyPolicy::None => None,
            ProxyPolicy::StaticHttp(proxy) => {
                let uri = proxy
                    .0
                    .as_str()
                    .parse()
                    .map_err(|_| TransportError::InvalidProxy)?;
                Some(Tunnel::new(uri, direct.clone()))
            }
        };
        Ok(Self {
            direct,
            proxy,
            #[cfg(test)]
            test_address: None,
        })
    }
}

impl Service<Uri> for Connector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(proxy) = &mut self.proxy {
            proxy.poll_ready(cx).map_err(Into::into)
        } else {
            self.direct.poll_ready(cx).map_err(Into::into)
        }
    }

    fn call(&mut self, destination: Uri) -> Self::Future {
        if let Some(proxy) = &mut self.proxy {
            let connecting = proxy.call(destination);
            return Box::pin(async move { connecting.await.map_err(Into::into) });
        }
        #[cfg(test)]
        let destination = if let Some(address) = self.test_address {
            // Change only the TCP destination: TLS SNI, Host and SDK URL stay fixed.
            format!("http://{address}")
                .parse()
                .expect("the test socket address is a valid HTTP authority")
        } else {
            destination
        };
        let connecting = self.direct.call(destination);
        Box::pin(async move { connecting.await.map_err(Into::into) })
    }
}
