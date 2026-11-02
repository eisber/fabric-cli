// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual public resource/operation adapter probe with synthetic execution only.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, TokenCredential};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceMethod, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
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
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        Ok(AccessToken::new(
            "synthetic-query-consumer",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone)]
struct Executor {
    requests: Arc<Mutex<Vec<Value>>>,
    status: u16,
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.requests.lock().unwrap().push(json!({
            "method":match request.method() {ResourceMethod::Get=>"get",ResourceMethod::Post=>"post",ResourceMethod::Patch=>"patch",ResourceMethod::Delete=>"delete"},
            "url":request.url().as_str(),
            "body":if request.body().is_empty() {Value::Null} else {serde_json::from_slice(request.body()).unwrap()},
        }));
        Box::pin(std::future::ready(Ok(OperationResponse {
            status: self.status,
            body: Vec::new(),
            headers: BTreeMap::new(),
        })))
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("headerless response must not poll")
    }
}

async fn probe(input: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let segments = input["segments"]
        .as_array()
        .ok_or("missing segments")?
        .iter()
        .map(|value| value.as_str().ok_or("invalid segment"))
        .collect::<Result<Vec<_>, _>>()?;
    let method = match input["method"].as_str().ok_or("missing method")? {
        "get" => ResourceMethod::Get,
        "post" => ResourceMethod::Post,
        "patch" => ResourceMethod::Patch,
        "delete" => ResourceMethod::Delete,
        _ => return Err("invalid method".into()),
    };
    let body = if input["body"].is_null() {
        None
    } else {
        Some(input["body"].clone())
    };
    let mut operation =
        ResourceOperation::new(method, &segments, body, CompletionPolicy::PythonSynchronous)?;
    let pairs = input["query"]
        .as_array()
        .ok_or("missing query")?
        .iter()
        .map(|pair| {
            Ok::<_, Box<dyn std::error::Error>>((
                pair[0].as_str().ok_or("invalid key")?,
                pair[1].as_str().ok_or("invalid value")?,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !pairs.is_empty() {
        operation = operation.with_query(&pairs)?;
    }
    if input["hard_first"] == true {
        operation = operation.with_hard_delete()?;
    }
    for token in input["tokens"].as_array().ok_or("missing tokens")? {
        operation = operation.with_continuation_token(token.as_str().ok_or("invalid token")?)?;
    }
    if input["hard_last"] == true {
        operation = operation.with_hard_delete()?;
    }
    let credential = Credential;
    let selection = CredentialSelection::new("offline", "tenant", Audience::Fabric)?;
    let status = input["status"]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
        .ok_or("invalid status")?;
    let executor = Executor {
        requests: Arc::new(Mutex::new(Vec::new())),
        status,
    };
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let outcome = ResourceOperations::execute(&mut session, operation).await?;
    Ok(
        json!({"requests":*executor.requests.lock().unwrap(),"status":match outcome {
            ResourceOutcome::Completed{status,..}=>status,ResourceOutcome::Accepted{..}=>202,
        }}),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        println!("{}", probe(&serde_json::from_str(&line?)?).await?);
    }
    Ok(())
}
