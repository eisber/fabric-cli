// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reusable offline OneLake command/SDK/shared HTTP consumer; never live auth.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_mutations::OneLakeReadHost;
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_onelake::{OneLakeClient, OneLakeLimits, OneLakePath};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_workspaces::CredentialSelection;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    io::{self, BufRead},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://storage.azure.com/.default"]);
        Ok(AccessToken::new(
            "synthetic-offline-onelake-command",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Value>>,
}
#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(json!({
            "method": match request.method() {
                Method::Get => "get",
                _ => panic!("unexpected non-GET OneLake read"),
            },
            "url": request.url().as_str(),
        }));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected OneLake read/retry"))
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        assert_eq!(delay, Duration::from_secs(240));
        Box::pin(pending())
    }
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut replies = VecDeque::new();
    for reply in input["replies"].as_array().ok_or("missing replies")? {
        let status = reply["status"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or("invalid status")?;
        let mut headers = Headers::new();
        if let Some(values) = reply["headers"].as_object() {
            for (name, value) in values {
                headers.insert(
                    name.clone(),
                    value.as_str().ok_or("invalid header")?.to_owned(),
                );
            }
        }
        replies.push_back(AsyncRawResponse::from_bytes(
            StatusCode::from(status),
            headers,
            serde_json::to_vec(&reply["data"])?,
        ));
    }
    let network = Arc::new(Network {
        replies: Mutex::new(replies),
        requests: Mutex::new(Vec::new()),
    });
    let http = Arc::new(HttpExecutor::new(
        network.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )?);
    let adapter = OneLakeHttpExecutor::new(http);
    let client = OneLakeClient::new(
        Arc::new(Credential),
        CredentialSelection::new(
            "offline",
            "tenant",
            fabric_sdk_http::PublicOrigin::OneLake.audience(),
        )?,
    );
    let host = OneLakeReadHost {
        client: &client,
        executor: &adapter,
        limits: OneLakeLimits::default(),
    };
    let path = OneLakePath::new(
        "workspace",
        "item",
        input["path"].as_str().ok_or("missing path")?,
    )?;
    let result = match input["command"].as_str().ok_or("missing command")? {
        "ls" => {
            host.list(
                &path,
                input["long"].as_bool().unwrap_or(false),
                input["query"].as_str(),
                pending(),
            )
            .await
        }
        "exists" => host.exists(&path, pending()).await,
        _ => return Err("unsupported OneLake example command".into()),
    };
    Ok(match result {
        Ok(result) => json!({
            "requests": *network.requests.lock().unwrap(),
            "data": result.output.data,
            "message": result.output.message,
            "show_headers": result.show_headers,
            "error": null,
        }),
        Err(error) => {
            let output = error.output();
            json!({
                "requests": *network.requests.lock().unwrap(),
                "data": null, "message": null, "show_headers": false, "error": output.error_code,
            })
        }
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?).await?);
    }
    Ok(())
}
