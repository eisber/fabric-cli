// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod shared;

use fabric_cli::{
    families::{FamilyOutput, FamilySession, transfer_files::TokioTransferFiles},
    parser::parse_args,
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext, PathElement};
use fabric_cli_local::{ConfigStore, NavigationState};
use fabric_cli_mutations::{CommandFuture, MutationUi, NoticeKind};
use fabric_cli_output::Timestamp;
use fabric_cli_transfer::TransferUi;
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use serde_json::{Value, json};
pub use shared::{BrokenWriter, Credential, Effects, Http, TransferUiCapture};
use std::{
    fs,
    future::{Future, pending},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
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

pub struct Harness {
    pub root: PathBuf,
    pub credential: Arc<Credential>,
    pub http: Arc<Http>,
    pub effects: Effects,
    pub(super) endpoint: EndpointBinding,
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
            endpoint: EndpointBinding::public(ServiceOrigin::Fabric),
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

    pub async fn execute(&mut self, args: &[&str]) -> FamilyOutput {
        self.execute_with_cancelled(args, pending()).await
    }

    pub async fn execute_with_cancelled(
        &mut self,
        args: &[&str],
        cancelled: impl Future<Output = ()> + Send,
    ) -> FamilyOutput {
        self.execute_with_capabilities(args, cancelled, &TransferUiCapture::default())
            .await
    }

    pub(super) async fn execute_with_capabilities(
        &mut self,
        args: &[&str],
        cancelled: impl Future<Output = ()> + Send,
        transfer_ui: &dyn TransferUi,
    ) -> FamilyOutput {
        self.execute_with_capabilities_and_deploy_inputs(
            args,
            cancelled,
            transfer_ui,
            &fabric_cli::deploy::DeployInputs::default(),
        )
        .await
    }

    pub(super) async fn execute_with_capabilities_and_deploy_inputs(
        &mut self,
        args: &[&str],
        cancelled: impl Future<Output = ()> + Send,
        transfer_ui: &dyn TransferUi,
        inputs: &fabric_cli::deploy::DeployInputs,
    ) -> FamilyOutput {
        let invocation = parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap();
        let sdk = Client::with_context(RequestContext::with_endpoint(
            self.credential.clone(),
            CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
            self.endpoint.clone(),
        ));
        let executor = HttpExecutor::new(
            self.http.clone(),
            Arc::new(Timer),
            ExecutionOptions::default(),
        )
        .unwrap()
        .with_endpoint(self.endpoint.clone());
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
        let mut files = TokioTransferFiles::new(
            self.root.clone(),
            None,
            Default::default(),
            tokio::runtime::Handle::current(),
        )
        .await
        .unwrap();
        let output = FamilySession {
            reads: &reads,
            http: &executor,
            store: &store,
            navigation: &mut navigation,
            local: &mut self.effects,
            mutations: &mut mutations,
            confirmation: None,
            transfer_ui,
            transfer_files: Some(&mut files),
            resource_files: None,
        }
        .dispatch_with_deploy_inputs(&invocation, inputs, cancelled)
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
