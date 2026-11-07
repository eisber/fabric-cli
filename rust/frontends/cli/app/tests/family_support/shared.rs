// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_local::{BrowserIdentity, Capacity, LocalEffects, LocalError};
use fabric_cli_output::CommandError;
use fabric_cli_transfer::{TransferEvent, TransferFuture, TransferUi};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    sync::Mutex,
};

#[derive(Debug, Default)]
pub struct Credential(pub Mutex<Vec<Vec<String>>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).into()).collect());
        Ok(AccessToken::new(
            "offline-family-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

type Reply = (u16, Value, BTreeMap<String, String>);

#[derive(Debug)]
pub struct Http {
    pub replies: Mutex<VecDeque<Reply>>,
    pub requests: Mutex<Vec<Value>>,
}

#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().scheme(), "https");
        assert!(request.headers().get_str(&"authorization".into()).is_ok());
        let body = match request.body() {
            Body::Bytes(bytes) if bytes.is_empty() => Value::Null,
            Body::Bytes(bytes) => {
                serde_json::from_slice(bytes).unwrap_or_else(|_| json!({"raw": bytes.to_vec()}))
            }
            _ => panic!("unexpected streaming family request"),
        };
        let raw_body = match request.body() {
            Body::Bytes(bytes) => String::from_utf8(bytes.to_vec()).unwrap(),
            _ => panic!("unexpected streaming family request"),
        };
        self.requests.lock().unwrap().push(json!({
            "method": request.method().to_string(), "url": request.url().as_str(), "body": body, "raw_body": raw_body,
        }));
        let (status, body, supplied) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP or mutation replay");
        let mut headers = Headers::new();
        for (key, value) in supplied {
            headers.insert(key, value);
        }
        let bytes = match body {
            Value::String(raw) => raw.into_bytes(),
            Value::Null => Vec::new(),
            value => serde_json::to_vec(&value).unwrap(),
        };
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::from(status),
            headers,
            bytes,
        ))
    }
}

#[derive(Default)]
pub struct Effects {
    pub browser: Vec<String>,
    pub clears: usize,
    pub cache_clears: usize,
}

impl LocalEffects for Effects {
    fn clear_cli_cache(&mut self) -> Result<(), LocalError> {
        self.cache_clears += 1;
        Ok(())
    }
    fn capacities(&mut self) -> Result<Vec<Capacity>, LocalError> {
        Ok(vec![Capacity {
            display_name: "Chosen".into(),
            id: "cap".into(),
        }])
    }
    fn browser_identity(&self) -> Result<BrowserIdentity, LocalError> {
        Ok(BrowserIdentity {
            user: true,
            claims: vec![
                ("upn".into(), "user@example.test".into()),
                ("tid".into(), "tenant".into()),
            ],
        })
    }
    fn open_browser(&mut self, url: &str) -> Result<(), LocalError> {
        self.browser.push(url.into());
        Ok(())
    }
    fn clear_terminal(&mut self) -> Result<(), LocalError> {
        self.clears += 1;
        Ok(())
    }
}

#[derive(Default)]
pub struct TransferUiCapture(Mutex<Vec<TransferEvent>>);

impl TransferUi for TransferUiCapture {
    fn emit(&self, event: TransferEvent) -> Result<(), CommandError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
    fn confirm<'a>(&'a self, _: &'a str) -> TransferFuture<'a, Result<bool, CommandError>> {
        Box::pin(async { Ok(true) })
    }
    fn select<'a>(
        &'a self,
        names: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>> {
        Box::pin(async { Ok(names.to_vec()) })
    }
}

pub struct BrokenWriter;

impl io::Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected writer failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
