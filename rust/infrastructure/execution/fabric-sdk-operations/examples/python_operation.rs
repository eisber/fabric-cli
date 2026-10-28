// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline original-Python operation-policy comparison consumer.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, TokenCredential};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::CompletionPolicy;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    io::{self, BufRead},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new(
            "synthetic-source-operation",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone)]
struct Executor {
    replies: Arc<Mutex<VecDeque<OperationResponse>>>,
    requests: Arc<Mutex<Vec<String>>>,
    waits: Arc<Mutex<Vec<u64>>>,
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.requests
            .lock()
            .unwrap()
            .push(request.url().path().trim_start_matches("/v1/").to_owned());
        let response = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected source operation request");
        Box::pin(std::future::ready(Ok(response)))
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.lock().unwrap().push(delay.as_secs());
        Box::pin(std::future::ready(Ok(())))
    }
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut replies = input["replies"]
        .as_array()
        .ok_or("missing replies")?
        .iter()
        .map(|reply| {
            Ok(OperationResponse {
                status: reply["status"]
                    .as_u64()
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or("invalid status")?,
                body: serde_json::to_vec(&reply["data"])?,
                headers: reply
                    .get("headers")
                    .and_then(Value::as_object)
                    .map(|headers| {
                        headers
                            .iter()
                            .map(|(key, value)| {
                                value
                                    .as_str()
                                    .map(|value| (key.clone(), value.into()))
                                    .ok_or("invalid header")
                            })
                            .collect::<Result<BTreeMap<_, _>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default(),
            })
        })
        .collect::<Result<VecDeque<_>, Box<dyn std::error::Error>>>()?;
    let initial = replies
        .pop_front()
        .ok_or("missing initial accepted response")?;
    let executor = Executor {
        replies: Arc::new(Mutex::new(replies)),
        requests: Arc::new(Mutex::new(Vec::new())),
        waits: Arc::new(Mutex::new(Vec::new())),
    };
    let credential = Credential;
    let selection = CredentialSelection::new("offline", "tenant", Audience::Fabric)?;
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = session
        .reconcile_accepted(initial, CompletionPolicy::PythonSynchronous)
        .await;
    let (status, data, error) = match result {
        Ok(response) => (
            Some(response.status),
            Some(serde_json::from_slice::<Value>(&response.body)?),
            None,
        ),
        Err(error) => (
            None,
            None,
            Some(match error.kind {
                OperationErrorKind::OperationFailed { status, .. } if status == "Cancelled" => {
                    "LongRunningOperationCancelled".to_owned()
                }
                OperationErrorKind::OperationFailed { .. } => {
                    "LongRunningOperationFailed".to_owned()
                }
                OperationErrorKind::Http { code, .. } => code.unwrap_or_else(|| "Http".into()),
                OperationErrorKind::InvalidLink => "InvalidLink".into(),
                OperationErrorKind::InvalidResponse => "InvalidResponse".into(),
                _ => return Err("unexpected operation classification".into()),
            }),
        ),
    };
    Ok(json!({
        "status": status, "data": data, "error": error,
        "requests": executor.requests.lock().unwrap().iter().cloned().collect::<Vec<_>>(),
        "waits": *executor.waits.lock().unwrap(),
        "fallbacks": session.warnings().len(),
    }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let input = serde_json::from_str(&line?)?;
        println!("{}", probe(&input).await?);
    }
    Ok(())
}
