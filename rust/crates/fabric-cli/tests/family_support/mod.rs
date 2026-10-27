// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    families::{FamilyOutput, FamilySession},
    parser::parse_args,
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext, PathElement};
use fabric_cli_local::{
    BrowserIdentity, Capacity, ConfigStore, LocalEffects, LocalError, NavigationState,
};
use fabric_cli_mutations::{CommandFuture, MutationUi, NoticeKind};
use fabric_cli_output::{CommandError, Timestamp};
use fabric_cli_transfer::{LocalTransferFiles, TransferEvent, TransferFuture, TransferUi};
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    future::pending,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
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

#[derive(Debug)]
pub struct Http {
    pub replies: Mutex<VecDeque<(u16, Value, BTreeMap<String, String>)>>,
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
        self.requests.lock().unwrap().push(json!({
            "method": request.method().to_string(), "url": request.url().as_str(), "body": body,
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

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
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
struct MutationUiCapture(Vec<(NoticeKind, String)>);
impl MutationUi for MutationUiCapture {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        self.0.push((kind, message.into()));
    }
    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        Box::pin(async { true })
    }
}

#[derive(Default)]
struct TransferUiCapture(Mutex<Vec<TransferEvent>>);
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

pub struct Harness {
    pub root: PathBuf,
    pub credential: Arc<Credential>,
    pub http: Arc<Http>,
    pub effects: Effects,
}

impl Harness {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fabric-family-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let store = ConfigStore::new(&root);
        let mut settings = store.initialize().unwrap();
        settings.insert("output_format".into(), json!("json"));
        store.write(&settings).unwrap();
        Self {
            root,
            credential: Arc::default(),
            http: Arc::new(Http {
                replies: Mutex::default(),
                requests: Mutex::default(),
            }),
            effects: Effects::default(),
        }
    }

    pub fn reply(&self, status: u16, body: Value) {
        self.headers(status, body, &[]);
    }

    pub fn headers(&self, status: u16, body: Value, headers: &[(&str, &str)]) {
        self.http.replies.lock().unwrap().push_back((
            status,
            body,
            headers
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
        ));
    }

    pub fn metadata(&self, items: Value) {
        self.reply(
            200,
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        );
        self.reply(200, json!({"value":items}));
    }

    pub fn item(&self, kind: &str) {
        self.metadata(json!([{"id":"n","displayName":"Book","type":kind}]));
    }

    pub async fn execute(&mut self, args: &[&str]) -> FamilyOutput {
        let invocation = parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap();
        let sdk = Client::new(
            self.credential.clone(),
            CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
        );
        let executor = HttpExecutor::new(
            self.http.clone(),
            Arc::new(Timer),
            ExecutionOptions::default(),
        )
        .unwrap();
        let tenant = Element::tenant("Tenant", "tenant");
        let reads = ReadSession {
            sdk: &sdk,
            executor: &executor,
            tenant: tenant.clone(),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2026-10-27T07:00:00Z").unwrap(),
            config: Default::default(),
        };
        let store = ConfigStore::new(&self.root);
        let mut navigation = NavigationState::new(
            PathElement::from_fabric(tenant),
            HomeContext::Unspecified,
            self.root.to_string_lossy().into_owned(),
            37,
            true,
        )
        .unwrap();
        let mut mutations = MutationUiCapture::default();
        let transfer_ui = TransferUiCapture::default();
        let mut files =
            LocalTransferFiles::new(self.root.clone(), None, Default::default()).unwrap();
        let output = FamilySession {
            reads: &reads,
            http: &executor,
            store: &store,
            navigation: &mut navigation,
            local: &mut self.effects,
            mutations: &mut mutations,
            confirmation: None,
            transfer_ui: &transfer_ui,
            transfer_files: Some(&mut files),
            resource_files: None,
        }
        .dispatch(&invocation, pending())
        .await
        .unwrap();
        assert!(
            self.http.replies.lock().unwrap().is_empty(),
            "unused responses for {args:?}: {:?}; output: {:?}; evidence: {:?}",
            self.http.requests.lock().unwrap(),
            output.plan,
            output.evidence
        );
        output
    }

    pub fn requests(&self) -> Vec<Value> {
        self.http.requests.lock().unwrap().clone()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

pub fn success(output: &FamilyOutput) -> String {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        output.write_to(&mut stdout, &mut stderr).unwrap(),
        0,
        "{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    String::from_utf8(stdout).unwrap()
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
