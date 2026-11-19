// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct SDK consumer: the real production adapter with synthetic injected
//! credentials and HTTP. No live query, login or credential discovery occurs.

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk::runtime::NativeDeadline;
use fabric_sdk_http::{ExecutionFuture, ExecutionTimer};
use fabric_sdk_query::{
    EngineBinding, KqlQuery, KustoCredentialContext, KustoQueryExecutor, QueryLimits, query_kql,
};
use fabric_v2_contracts::ItemIdentity;
use std::{future::pending, sync::Arc, time::Duration};

const SCOPE: &str = "https://api.kusto.windows.net/.default";

#[derive(Debug)]
struct SyntheticCredential;

#[async_trait]
impl TokenCredential for SyntheticCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [SCOPE]);
        Ok(AccessToken::new(
            "synthetic-token",
            OffsetDateTime::from_unix_timestamp(4_102_444_800).unwrap(),
        ))
    }
}

#[derive(Debug)]
struct SyntheticHttp;

#[async_trait]
impl HttpClient for SyntheticHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.method(), Method::Post);
        assert_eq!(
            request.url().as_str(),
            "https://synthetic.kusto.fabric.microsoft.com/v2/rest/query"
        );
        let body = Bytes::from(request.body());
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["db"], "explicit-database");
        assert_eq!(
            body["properties"]["Options"]["request_readonly_hardline"],
            true
        );
        let bytes = Bytes::from_static(
            br#"[
                {"FrameType":"DataSetHeader","Version":"v2.0","IsProgressive":false},
                {"FrameType":"DataTable","TableId":0,"TableKind":"PrimaryResult",
                 "TableName":"Synthetic","Columns":[{"ColumnName":"value","ColumnType":"long"}],
                 "Rows":[[9007199254740993]]},
                {"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":false}
            ]"#,
        );
        let mut headers = Headers::new();
        headers.insert("content-type", "application/json; charset=utf-8");
        Ok(AsyncRawResponse::new(
            200.into(),
            headers,
            Box::pin(futures_util::stream::iter([Ok(bytes)])),
        ))
    }
}

struct CallerTimer;

impl ExecutionTimer for CallerTimer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binding = EngineBinding::kusto(
        "caller-resolved-kql",
        "1",
        ItemIdentity::new(
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
        )?,
        Url::parse("https://synthetic.kusto.fabric.microsoft.com/")?,
        "explicit-database",
        SCOPE,
        "explicit-account",
        "explicit-tenant",
    )?;
    let credentials = KustoCredentialContext::new(
        Arc::new(SyntheticCredential),
        "explicit-account",
        "explicit-tenant",
        SCOPE,
    )?;
    let executor = KustoQueryExecutor::new(
        binding.clone(),
        credentials,
        Arc::new(SyntheticHttp),
        Arc::new(CallerTimer),
    )?;
    let deadline = NativeDeadline::new(Duration::from_secs(30))?;
    let query = KqlQuery::new("print value=9007199254740993", "query;synthetic-example")?
        .with_server_timeout(Duration::from_secs(1))?;
    let limits = QueryLimits::new(4096, 1024 * 1024, 100, 8, 32, 32)?;
    let result = query_kql(&binding, &executor, &query, limits, &deadline, pending()).await?;
    assert!(result.evidence.completed && result.evidence.http_eof);
    assert!(!result.evidence.production_verified);
    println!("Complete native result sets: {}", result.datasets.len());
    Ok(())
}
