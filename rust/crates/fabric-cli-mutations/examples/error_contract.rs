// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic SDK/CLI/error-output consumer for original Python comparison.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_mutations::operation_error_output;
use fabric_cli_output::{Indent, OutputEnvelope, Timestamp};
use fabric_sdk_operations::{
    Audience, FabricOperation, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::{
    CredentialSelection,
    mutations::{CompletionPolicy, ResourceMethod},
};
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
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new(
            "synthetic-error-contract-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone)]
struct Executor {
    replies: Arc<Mutex<VecDeque<OperationResponse>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.requests.lock().unwrap().push(json!({
            "method": match request.method() {
                ResourceMethod::Get => "get", ResourceMethod::Post => "post",
                ResourceMethod::Patch => "patch", ResourceMethod::Delete => "delete",
            },
            "url": request.url().as_str(),
        }));
        let response = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected error-contract request");
        Box::pin(std::future::ready(Ok(response)))
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let mut replies = VecDeque::new();
    for reply in input["replies"].as_array().ok_or("missing replies")? {
        let headers = reply
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
            .unwrap_or_default();
        replies.push_back(OperationResponse {
            status: reply["status"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .ok_or("invalid status")?,
            body: match reply.get("text").and_then(Value::as_str) {
                Some(text) => text.as_bytes().to_vec(),
                None => serde_json::to_vec(&reply["data"])?,
            },
            headers,
        });
    }
    let executor = Executor {
        replies: Arc::new(Mutex::new(replies)),
        requests: Arc::new(Mutex::new(Vec::new())),
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
    let error = session
        .execute(
            FabricOperation::new(
                ResourceMethod::Patch,
                "/v1/workspaces/ws/items/item",
                b"{}".to_vec(),
                CompletionPolicy::PythonSynchronous,
            )
            .map_err(|_| "invalid operation")?,
        )
        .await
        .expect_err("error contract unexpectedly completed");
    let plan = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z")?,
        operation_error_output(&error),
    )
    .render(Indent::Four)?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr)?;
    assert!(stderr.is_empty());
    let payload: Value = serde_json::from_slice(&stdout)?;
    Ok(json!({
        "code": payload["result"]["error_code"],
        "message": payload["result"]["message"],
        "exit": plan.exit_code,
        "requests": *executor.requests.lock().unwrap(),
    }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?).await?);
    }
    Ok(())
}
