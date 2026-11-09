// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic boundaries for the production binary's shared context hook.
#![deny(unnameable_test_items)]

use super::*;

#[test]
fn returned_failures_are_not_terminal_and_cancellation_does_not_depend_on_exit() {
    for exit in [0, 1, 2, 4] {
        assert_eq!(
            code_outcome(Some("ReadFailed"), exit),
            CommandOutcome::Returned(exit)
        );
        assert_eq!(
            BatchOutcome::from(code_outcome(Some("OperationCancelled"), exit)),
            BatchOutcome::Stop(exit)
        );
        assert_eq!(
            code_outcome(Some("OperationCancelled"), exit),
            CommandOutcome::Cancelled(exit)
        );
        assert_eq!(
            code_outcome(Some("UnexpectedError"), exit),
            CommandOutcome::Stop(exit)
        );
    }
}

#[test]
fn restore_selection_is_a_single_snapshot_and_its_notice_is_consumed_once() {
    let mut state = BatchState::new(Ok(("/Demo.Workspace".into(), true)));
    assert!(state.take_restored().unwrap());
    assert!(!state.take_restored().unwrap());
    assert_eq!(state.path().unwrap(), "/Demo.Workspace");
    let state = BatchState::new(Err(LocalError::command("InvalidInput", "Invalid selector")));
    assert_eq!(
        state.path().unwrap_err().command_error(),
        LocalError::command("InvalidInput", "Invalid selector").command_error()
    );
}

#[test]
fn zero_exit_cancellation_stops_before_the_next_command_and_summary() {
    use fabric_cli::parser::batch::{self, BatchHost, BatchInvocation};
    use std::convert::Infallible;

    struct Host {
        commands: usize,
        summarized: bool,
    }

    impl BatchHost for Host {
        type Error = Infallible;

        fn execute(&mut self, _: &[&str]) -> Result<BatchOutcome, Self::Error> {
            self.commands += 1;
            Ok(code_outcome(Some("OperationCancelled"), 0).into())
        }

        fn separator(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn summary(&mut self, _: &BatchInvocation) -> Result<(), Self::Error> {
            self.summarized = true;
            Ok(())
        }
    }

    let mut host = Host {
        commands: 0,
        summarized: false,
    };
    assert_eq!(
        batch::run(
            &BatchInvocation {
                commands: vec!["pwd".into(), "config set show_hidden true".into()],
                output_format: None,
            },
            &mut host,
        )
        .unwrap(),
        0
    );
    assert_eq!(host.commands, 1);
    assert!(!host.summarized);
}

#[cfg(feature = "native-host")]
#[test]
fn read_binding_is_directory_independent_and_later_navigation_uses_the_same_identity() {
    let mut state = BatchState::new(Ok(("/Demo.Workspace".into(), true)));
    state
        .bind_read_provider("tenant", "account", "client", "client-secret")
        .unwrap();
    assert!(state.navigation.is_none());
    assert_eq!(state.path().unwrap(), "/Demo.Workspace");
    state
        .bind_read_provider("tenant", "account", "client", "client-secret")
        .unwrap();
    assert!(state.navigation.is_none());
    state
        .bind_provider(
            "tenant",
            "account",
            "client",
            "client-secret",
            HomeContext::Unspecified,
        )
        .unwrap();
    assert!(state.navigation.is_some());
    assert!(state.take_restored().unwrap());
    assert!(!state.take_restored().unwrap());
}

#[cfg(feature = "native-host")]
#[test]
fn read_binding_rejects_selector_changes_before_navigation_initialization() {
    let mut state = BatchState::new(Ok(("/Demo.Workspace".into(), false)));
    state
        .bind_read_provider("tenant", "account", "client", "client-secret")
        .unwrap();
    let expected = state.binding.clone();
    for (tenant, account, client, provider) in [
        ("other-tenant", "account", "client", "client-secret"),
        ("tenant", "other-account", "client", "client-secret"),
        ("tenant", "account", "other-client", "client-secret"),
        ("tenant", "account", "client", "other-provider"),
    ] {
        let error = state
            .bind_read_provider(tenant, account, client, provider)
            .unwrap_err();
        assert_eq!(state.binding, expected);
        assert!(state.navigation.is_none());
        assert_eq!(state.path().unwrap(), "/Demo.Workspace");
        let diagnostic = format!("{error:?}: {error}");
        for value in [tenant, account, client, provider] {
            assert!(!diagnostic.contains(value));
        }
    }
}

#[cfg(feature = "native-host")]
#[test]
fn credential_binding_rejects_each_selector_change_without_losing_navigation_or_leaking_values() {
    let mut state = BatchState::new(Ok(("/Demo.Workspace".into(), false)));
    let home = HomeContext::Unspecified;
    state
        .bind_provider("tenant", "account", "client", "client-secret", home.clone())
        .unwrap();
    let current = Arc::clone(state.navigation.as_ref().unwrap().current());
    state
        .bind_provider("tenant", "account", "client", "client-secret", home.clone())
        .unwrap();
    assert!(Arc::ptr_eq(
        &current,
        state.navigation.as_ref().unwrap().current()
    ));
    for (tenant, account, client, provider) in [
        ("other-tenant", "account", "client", "client-secret"),
        ("tenant", "other-account", "client", "client-secret"),
        ("tenant", "account", "other-client", "client-secret"),
        ("tenant", "account", "client", "managed-identity-vm"),
        (
            "tenant",
            "account",
            "client",
            r"client-assertion:C:\sensitive-selector\assertion",
        ),
        (
            "tenant",
            "account",
            "client",
            "managed-identity-app-service:http://127.0.0.1:42/msi/token",
        ),
    ] {
        let error = state
            .bind_provider(tenant, account, client, provider, home.clone())
            .unwrap_err();
        let diagnostic = format!("{error:?}: {error}");
        for value in [tenant, account, client, provider] {
            assert!(!diagnostic.contains(value), "{diagnostic}");
        }
        assert_eq!(state.path().unwrap(), "/Demo.Workspace");
        assert!(Arc::ptr_eq(
            &current,
            state.navigation.as_ref().unwrap().current()
        ));
    }
}

#[cfg(feature = "native-host")]
#[test]
fn selected_assertion_path_and_managed_identity_endpoint_cannot_change_within_one_mode() {
    for (provider, changed) in [
        (
            r"client-assertion:C:\selected\assertion",
            r"client-assertion:C:\changed\assertion",
        ),
        (
            r"workload-identity:C:\selected\federated-token",
            r"workload-identity:C:\changed\federated-token",
        ),
        (
            "managed-identity-app-service:http://127.0.0.1:42/msi/token",
            "managed-identity-app-service:http://127.0.0.1:43/msi/token",
        ),
    ] {
        let mut state = BatchState::new(Ok(("/Demo.Workspace".into(), false)));
        state
            .bind_provider(
                "tenant",
                "account",
                "client",
                provider,
                HomeContext::Unspecified,
            )
            .unwrap();
        let current = Arc::clone(state.navigation.as_ref().unwrap().current());
        let error = state
            .bind_provider(
                "tenant",
                "account",
                "client",
                changed,
                HomeContext::Unspecified,
            )
            .unwrap_err();
        assert!(!format!("{error:?}: {error}").contains(changed));
        assert!(!format!("{error:?}: {error}").contains(provider));
        assert_eq!(state.path().unwrap(), "/Demo.Workspace");
        assert!(Arc::ptr_eq(
            &current,
            state.navigation.as_ref().unwrap().current()
        ));
    }
}

#[cfg(feature = "native-host")]
mod remote {
    use super::*;
    use async_trait::async_trait;
    use azure_core::{
        credentials::{AccessToken, TokenCredential, TokenRequestOptions},
        http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
        time::OffsetDateTime,
    };
    use fabric_cli::{
        families::{FamilyOutput, FamilySession},
        parser::{
            Invocation,
            batch::{self, BatchHost, BatchInvocation},
            parse_args,
        },
        session::ReadSession,
    };
    use fabric_cli_local::{ConfigStore, LocalEffects};
    use fabric_cli_mutations::{CommandFuture, MutationUi, NoticeKind};
    use fabric_cli_output::{CommandError, Timestamp, text::TextPlan};
    use fabric_cli_transfer::{TransferEvent, TransferFuture, TransferUi};
    use fabric_sdk::{Audience, Client, CredentialSelection};
    use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
    use fabric_sdk_operations::{
        OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
        OperationResponse,
    };
    use serde_json::{Value, json};
    use std::{
        collections::{BTreeMap, VecDeque},
        fs,
        future::{Future, pending, poll_fn},
        io,
        path::PathBuf,
        pin::Pin,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::Poll,
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
            Ok(AccessToken::new("offline-only", OffsetDateTime::UNIX_EPOCH))
        }
    }

    #[derive(Debug, Default)]
    struct Http {
        replies: Mutex<VecDeque<Value>>,
        requests: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl HttpClient for Http {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            self.requests
                .lock()
                .unwrap()
                .push(request.url().to_string());
            let body = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected HTTP or replay");
            Ok(AsyncRawResponse::from_bytes(
                StatusCode::Ok,
                Headers::new(),
                serde_json::to_vec(&body).unwrap(),
            ))
        }
    }
    struct Timer;
    impl ExecutionTimer for Timer {
        fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
            Box::pin(pending())
        }
    }

    struct Ui;
    impl LocalEffects for Ui {
        fn clear_cli_cache(&mut self) -> Result<(), LocalError> {
            Ok(())
        }
        fn capacities(&mut self) -> Result<Vec<fabric_cli_local::Capacity>, LocalError> {
            Ok(Vec::new())
        }
    }
    impl MutationUi for Ui {
        fn notice(&mut self, _: NoticeKind, _: &str) {}
        fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
            Box::pin(async { true })
        }
    }
    impl TransferUi for Ui {
        fn emit(&self, _: TransferEvent) -> Result<(), CommandError> {
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

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = PathBuf::from("target").join(format!(
                "batch-context-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    struct Host {
        state: BatchState,
        store: ConfigStore,
        runtime: tokio::runtime::Runtime,
        http: Arc<Http>,
        sdk: Client,
        executor: HttpExecutor,
        plans: Vec<TextPlan>,
        summary: bool,
    }

    impl Host {
        fn new(root: &Root, restored: bool) -> Self {
            let store = ConfigStore::new(&root.0);
            store.initialize().unwrap();
            let http = Arc::new(Http::default());
            let sdk = Client::new(
                Arc::new(Credential),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            );
            let executor =
                HttpExecutor::new(http.clone(), Arc::new(Timer), ExecutionOptions::default())
                    .unwrap();
            let mut state = BatchState::new(Ok(("/".into(), restored)));
            state
                .bind("tenant", "offline", "client", HomeContext::Unspecified)
                .unwrap();
            Self {
                state,
                store,
                runtime: tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap(),
                http,
                sdk,
                executor,
                plans: Vec::new(),
                summary: false,
            }
        }

        fn workspace(&self) {
            self.http
                .replies
                .lock()
                .unwrap()
                .push_back(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}));
        }
    }

    impl BatchHost for Host {
        type Error = io::Error;
        fn execute(&mut self, arguments: &[&str]) -> io::Result<BatchOutcome> {
            if arguments == ["cancel"] {
                let error = fabric_cli_mutations::MutationError::new(
                    "OperationCancelled",
                    "Synthetic cancellation",
                );
                return Ok(family_outcome(&FamilyOutput {
                    plan: TextPlan {
                        records: Vec::new(),
                        exit_code: 1,
                    },
                    evidence: fabric_cli::families::FamilyEvidence::Resource(Err(error)),
                })
                .into());
            }
            let invocation =
                parse_args(std::iter::once("fab-rs").chain(arguments.iter().copied())).unwrap();
            let path = self.state.path().unwrap();
            let reads = ReadSession {
                sdk: &self.sdk,
                executor: &self.executor,
                tenant: Element::tenant("tenant", "tenant"),
                current_path: &path,
                home: HomeContext::Unspecified,
                timestamp: Timestamp::parse("2026-10-29T12:00:00Z").unwrap(),
                config: Default::default(),
            };
            let plan = if let Invocation::Read(command) = &invocation {
                self.runtime
                    .block_on(reads.dispatch(command, pending()))
                    .unwrap()
            } else {
                let output = self.runtime.block_on(self.state.family(async |navigation| {
                    FamilySession {
                        reads: &reads,
                        http: &self.executor,
                        store: &self.store,
                        navigation,
                        local: &mut Ui,
                        mutations: &mut Ui,
                        confirmation: None,
                        transfer_ui: &Ui,
                        transfer_files: None,
                        resource_files: None,
                    }
                    .dispatch(&invocation, pending())
                    .await
                    .unwrap()
                }));
                let outcome = family_outcome(&output);
                self.plans.push(output.plan);
                return Ok(outcome.into());
            };
            let outcome = BatchOutcome::Returned(plan.exit_code);
            self.plans.push(plan);
            Ok(outcome)
        }
        fn separator(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn summary(&mut self, _: &BatchInvocation) -> io::Result<()> {
            self.summary = true;
            Ok(())
        }
    }

    fn run(host: &mut Host, commands: &[&str]) -> u8 {
        batch::run(
            &BatchInvocation {
                commands: commands.iter().map(|value| (*value).into()).collect(),
                output_format: None,
            },
            host,
        )
        .unwrap()
    }

    #[test]
    fn remote_cd_relative_get_pwd_and_config_changes_share_context_without_persistence() {
        let root = Root::new();
        let mut host = Host::new(&root, true);
        host.workspace(); // cd
        host.workspace(); // relative get's current context
        host.http
            .replies
            .lock()
            .unwrap()
            .push_back(json!({"id":"w","displayName":"Demo"}));
        host.http.replies.lock().unwrap().extend([
            json!({"value":[]}),
            json!({}),
            json!({"value":[]}),
        ]);
        host.workspace(); // pwd
        host.workspace(); // config update
        host.workspace(); // pwd after settings change
        assert_eq!(
            run(
                &mut host,
                &[
                    "cd Demo.Workspace",
                    "get . -q id",
                    "pwd",
                    "config set show_hidden true",
                    "pwd"
                ]
            ),
            0
        );
        assert_eq!(host.state.path().unwrap(), "/Demo.Workspace");
        assert!(host.summary);
        assert!(host.state.take_restored().unwrap());
        assert!(!host.state.take_restored().unwrap());
        assert!(
            !root
                .0
                .join(format!(
                    "context-{}.json",
                    crate::local_host::session_id().unwrap()
                ))
                .exists()
        );
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
        assert_eq!(
            host.http
                .requests
                .lock()
                .unwrap()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "https://api.fabric.microsoft.com/v1/workspaces",
                "https://api.fabric.microsoft.com/v1/workspaces",
                "https://api.fabric.microsoft.com/v1/workspaces/w",
                "https://api.fabric.microsoft.com/v1/workspaces/w/managedPrivateEndpoints",
                "https://api.fabric.microsoft.com/v1/workspaces/w/spark/settings",
                "https://api.fabric.microsoft.com/v1/workspaces/w/roleAssignments",
                "https://api.fabric.microsoft.com/v1/workspaces",
                "https://api.fabric.microsoft.com/v1/workspaces",
                "https://api.fabric.microsoft.com/v1/workspaces",
            ]
        );
        assert!(host.http.replies.lock().unwrap().is_empty());
        assert_eq!(host.store.read().unwrap()["show_hidden"], "true");
    }

    #[test]
    fn handled_failure_continues_but_typed_cancellation_stops_before_mutation_and_summary() {
        let root = Root::new();
        let mut host = Host::new(&root, false);
        assert_eq!(
            run(
                &mut host,
                &["config get missing", "config set show_hidden true"]
            ),
            0
        );
        assert_eq!(host.store.read().unwrap()["show_hidden"], "true");
        host.summary = false;
        assert_eq!(
            run(&mut host, &["cancel", "config set show_hidden false"]),
            1
        );
        assert!(!host.summary);
        assert_eq!(host.store.read().unwrap()["show_hidden"], "true");
    }

    #[test]
    fn in_memory_context_survives_output_failure_and_binding_cannot_switch_tenant() {
        let root = Root::new();
        let mut host = Host::new(&root, false);
        host.workspace();
        assert_eq!(run(&mut host, &["cd Demo.Workspace"]), 0);
        struct Broken;
        impl io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        // A successful cd can have no output, so append a synthetic delivery
        // record without replaying or rolling back its completed state change.
        let mut plan = host.plans.pop().unwrap();
        plan.records.extend(
            fabric_cli_output::text::render_line(
                "delivery",
                fabric_cli_output::text::Style::Plain,
                fabric_cli_output::OutputStream::Stdout,
                Default::default(),
            )
            .records,
        );
        assert!(plan.write_to(&mut Broken, &mut Vec::new()).is_err());
        assert_eq!(host.state.path().unwrap(), "/Demo.Workspace");
        assert!(
            host.state
                .bind("other", "offline", "client", HomeContext::Unspecified)
                .is_err()
        );
        assert_eq!(host.state.path().unwrap(), "/Demo.Workspace");
        assert_eq!(host.http.requests.lock().unwrap().len(), 1);
    }

    #[derive(Clone, Copy, Debug)]
    enum FailureBoundary {
        Read,
        SourceRead,
        JobRead,
        JobExecution,
        JobMutation,
        Family,
    }

    impl fabric_cli_jobs::JobUi for Ui {
        fn emit(&self, _: fabric_cli_jobs::JobEvent) -> Result<(), CommandError> {
            Ok(())
        }

        fn confirm(&self) -> fabric_cli_jobs::Confirmation<'_> {
            Box::pin(async { Ok(true) })
        }
    }

    struct FailureHost {
        inner: Host,
        boundary: FailureBoundary,
        invalid_context: bool,
        calls: usize,
        mutated: bool,
        observed: Option<CommandOutcome>,
    }

    impl FailureHost {
        fn fail_metadata(&mut self) -> CommandOutcome {
            let path = self.inner.state.path().unwrap();
            let invalid_context = self.invalid_context;
            let cancelled = || {
                if invalid_context {
                    futures_util::future::Either::Left(pending::<()>())
                } else {
                    futures_util::future::Either::Right(std::future::ready(()))
                }
            };
            let reads = ReadSession {
                sdk: &self.inner.sdk,
                executor: &self.inner.executor,
                tenant: Element::tenant(
                    "tenant",
                    if invalid_context {
                        "unselected"
                    } else {
                        "tenant"
                    },
                ),
                current_path: &path,
                home: HomeContext::Unspecified,
                timestamp: timestamp(),
                config: Default::default(),
            };
            assert_eq!(
                reads.tenant.id() != Some(reads.sdk.selection().tenant()),
                invalid_context,
            );
            let expected = if self.invalid_context {
                "InvalidInput"
            } else {
                "LongRunningOperationCancelled"
            };
            let check_error = |error: &CommandError| {
                assert_eq!(
                    error.envelope(timestamp(), None).result().error_code(),
                    Some(expected),
                    "{:?}",
                    self.boundary,
                );
            };
            let (plan, outcome) = match self.boundary {
                FailureBoundary::Read | FailureBoundary::SourceRead => {
                    let Invocation::Read(command) =
                        parse_args(["fab-rs", "get", ".", "-q", "id"]).unwrap()
                    else {
                        panic!("read invocation");
                    };
                    let output = self
                        .inner
                        .runtime
                        .block_on(async {
                            if matches!(self.boundary, FailureBoundary::SourceRead) {
                                reads
                                    .with_source_operations(&self.inner.executor)
                                    .dispatch_with_evidence(&command, cancelled())
                                    .await
                            } else {
                                reads.dispatch_with_evidence(&command, cancelled()).await
                            }
                        })
                        .unwrap();
                    check_error(output.error.as_ref().expect("original read error"));
                    assert!(output.operation_error.is_none());
                    let outcome = read_outcome(&output);
                    (output.plan, outcome)
                }
                FailureBoundary::JobRead
                | FailureBoundary::JobExecution
                | FailureBoundary::JobMutation => {
                    let jobs = Arc::new(fabric_cli_jobs::HttpJobExecutor::new(
                        self.inner.executor.clone(),
                    ));
                    let command = fabric_cli::parser::JobReadCommand {
                        kind: fabric_cli::parser::JobReadKind::List,
                        path: vec!["Book.Notebook".into()],
                        id: None,
                        schedule: false,
                        output_format: Some("json".into()),
                        skill: None,
                    };
                    let output = if matches!(self.boundary, FailureBoundary::JobRead) {
                        self.inner.runtime.block_on(
                            fabric_cli::jobs::JobReadSession {
                                reads: &reads,
                                jobs,
                            }
                            .dispatch_with_evidence(&command, cancelled()),
                        )
                    } else {
                        let Invocation::JobRun(run) =
                            parse_args(["fab-rs", "job", "run", "Book.Notebook"]).unwrap()
                        else {
                            panic!("job invocation");
                        };
                        let mut command = run.family_command();
                        if matches!(self.boundary, FailureBoundary::JobMutation) {
                            command.kind = fabric_cli_jobs::JobKind::Cancel;
                            command.id = Some("run".into());
                        }
                        self.inner.runtime.block_on(
                            fabric_cli::jobs::JobSession {
                                reads: &reads,
                                jobs,
                                files: &fabric_cli_jobs::NoFiles,
                                ui: &Ui,
                                config: Default::default(),
                            }
                            .dispatch_with_evidence(&command, cancelled()),
                        )
                    }
                    .unwrap();
                    let fabric_cli_jobs::JobCommandError::Command(error) =
                        output.error.as_ref().expect("original job metadata error")
                    else {
                        panic!("metadata error was replaced by operation evidence");
                    };
                    check_error(error);
                    let outcome = job_outcome(&output);
                    (output.plan, outcome)
                }
                FailureBoundary::Family => {
                    let invocation = parse_args(["fab-rs", "pwd"]).unwrap();
                    let output =
                        self.inner
                            .runtime
                            .block_on(self.inner.state.family(async |navigation| {
                                FamilySession {
                                    reads: &reads,
                                    http: &self.inner.executor,
                                    store: &self.inner.store,
                                    navigation,
                                    local: &mut Ui,
                                    mutations: &mut Ui,
                                    confirmation: None,
                                    transfer_ui: &Ui,
                                    transfer_files: None,
                                    resource_files: None,
                                }
                                .dispatch(&invocation, cancelled())
                                .await
                                .unwrap()
                            }));
                    let fabric_cli::families::FamilyEvidence::CommandFailure(error) =
                        &output.evidence
                    else {
                        panic!("original family metadata error was discarded");
                    };
                    check_error(error);
                    let outcome = family_outcome(&output);
                    (output.plan, outcome)
                }
            };
            self.inner.plans.push(plan);
            self.observed = Some(outcome);
            outcome
        }
    }

    impl BatchHost for FailureHost {
        type Error = io::Error;

        fn execute(&mut self, arguments: &[&str]) -> io::Result<BatchOutcome> {
            self.calls += 1;
            if arguments == ["mutate"] {
                self.mutated = true;
                Ok(BatchOutcome::Returned(0))
            } else {
                Ok(self.fail_metadata().into())
            }
        }

        fn separator(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn summary(&mut self, _: &BatchInvocation) -> io::Result<()> {
            self.inner.summary = true;
            Ok(())
        }
    }

    #[test]
    fn actual_metadata_cancellation_stops_each_dispatch_before_http_next_command_and_summary() {
        for boundary in [
            FailureBoundary::Read,
            FailureBoundary::SourceRead,
            FailureBoundary::JobRead,
            FailureBoundary::JobExecution,
            FailureBoundary::JobMutation,
            FailureBoundary::Family,
        ] {
            for invalid_context in [false, true] {
                let root = Root::new();
                let mut host = FailureHost {
                    inner: Host::new(&root, false),
                    boundary,
                    invalid_context,
                    calls: 0,
                    mutated: false,
                    observed: None,
                };
                host.inner.state.initial = Ok(("/Demo.Workspace".into(), false));
                let exit = batch::run(
                    &BatchInvocation {
                        commands: vec!["metadata".into(), "mutate".into()],
                        output_format: None,
                    },
                    &mut host,
                )
                .unwrap();
                let failed_exit = host.inner.plans[0].exit_code;
                assert_eq!(
                    host.observed,
                    Some(if invalid_context {
                        CommandOutcome::Returned(failed_exit)
                    } else {
                        CommandOutcome::Cancelled(failed_exit)
                    }),
                    "{boundary:?}",
                );
                assert_eq!(exit, if invalid_context { 0 } else { failed_exit });
                assert_eq!(host.calls, if invalid_context { 2 } else { 1 });
                assert_eq!(host.mutated, invalid_context);
                assert_eq!(host.inner.summary, invalid_context);
                assert!(host.inner.http.requests.lock().unwrap().is_empty());
                assert_eq!(host.inner.state.path().unwrap(), "/Demo.Workspace");
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum OperationBoundary {
        PrimaryCooperative,
        PrimaryExecution,
        PolledTerminal,
        PolledExecution,
    }

    struct CancelOperation {
        boundary: OperationBoundary,
        requests: Mutex<Vec<String>>,
        cancelled: AtomicBool,
    }

    const OPERATION_ID: &str = "11111111-2222-3333-4444-555555555555";

    impl OperationExecutor for CancelOperation {
        fn execute<'a>(
            &'a self,
            request: OperationRequest<'a>,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a,
            >,
        > {
            let count = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.url().to_string());
                requests.len()
            };
            Box::pin(async move {
                match self.boundary {
                    OperationBoundary::PrimaryCooperative => {
                        poll_fn(|context| {
                            self.cancelled.store(true, Ordering::Release);
                            context.waker().wake_by_ref();
                            Poll::Pending
                        })
                        .await
                    }
                    OperationBoundary::PrimaryExecution => Err(OperationExecutionError::Cancelled),
                    OperationBoundary::PolledTerminal | OperationBoundary::PolledExecution
                        if count == 1 =>
                    {
                        Ok(OperationResponse {
                            status: 202,
                            body: Vec::new(),
                            headers: BTreeMap::from([
                                ("x-ms-operation-id".into(), OPERATION_ID.into()),
                                ("Retry-After".into(), "0".into()),
                            ]),
                        })
                    }
                    OperationBoundary::PolledTerminal => Ok(OperationResponse {
                        status: 200,
                        body: serde_json::to_vec(&json!({
                            "status":"Cancelled",
                            "error":{"errorCode":"SyntheticCancellation","message":"Stopped"}
                        }))
                        .unwrap(),
                        headers: BTreeMap::new(),
                    }),
                    OperationBoundary::PolledExecution => Err(OperationExecutionError::Cancelled),
                }
            })
        }

        fn wait(
            &self,
            _: Duration,
        ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    struct DispatchHost<F> {
        dispatch: F,
        calls: usize,
        mutated: bool,
        summary: bool,
    }

    impl<F: FnMut() -> CommandOutcome> BatchHost for DispatchHost<F> {
        type Error = io::Error;

        fn execute(&mut self, arguments: &[&str]) -> io::Result<BatchOutcome> {
            self.calls += 1;
            if arguments == ["mutate"] {
                self.mutated = true;
                Ok(BatchOutcome::Returned(0))
            } else {
                Ok((self.dispatch)().into())
            }
        }

        fn separator(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn summary(&mut self, _: &BatchInvocation) -> io::Result<()> {
            self.summary = true;
            Ok(())
        }
    }

    #[test]
    fn primary_and_polled_operation_cancellation_retain_evidence_and_stop_the_batch() {
        for boundary in [
            OperationBoundary::PrimaryCooperative,
            OperationBoundary::PrimaryExecution,
            OperationBoundary::PolledTerminal,
            OperationBoundary::PolledExecution,
        ] {
            let root = Root::new();
            let mut host = Host::new(&root, false);
            host.state.initial = Ok(("/Demo.Workspace".into(), false));
            host.workspace();
            let path = host.state.path().unwrap();
            let reads = ReadSession {
                sdk: &host.sdk,
                executor: &host.executor,
                tenant: Element::tenant("tenant", "tenant"),
                current_path: &path,
                home: HomeContext::Unspecified,
                timestamp: timestamp(),
                config: Default::default(),
            };
            let Invocation::Read(command) = parse_args(["fab-rs", "get", ".", "-q", "id"]).unwrap()
            else {
                panic!("read invocation");
            };
            let operations = CancelOperation {
                boundary,
                requests: Mutex::new(Vec::new()),
                cancelled: AtomicBool::new(false),
            };
            let mut retained = None;
            let exit = {
                let mut driver = DispatchHost {
                    dispatch: || {
                        let output = host
                            .runtime
                            .block_on(
                                reads
                                    .with_source_operations(&operations)
                                    .dispatch_with_evidence(
                                        &command,
                                        poll_fn(|_| {
                                            if operations.cancelled.load(Ordering::Acquire) {
                                                Poll::Ready(())
                                            } else {
                                                Poll::Pending
                                            }
                                        }),
                                    ),
                            )
                            .unwrap();
                        let outcome = read_outcome(&output);
                        assert_eq!(outcome, CommandOutcome::Cancelled(output.plan.exit_code));
                        retained = Some(output);
                        outcome
                    },
                    calls: 0,
                    mutated: false,
                    summary: false,
                };
                let exit = batch::run(
                    &BatchInvocation {
                        commands: vec!["get".into(), "mutate".into()],
                        output_format: None,
                    },
                    &mut driver,
                )
                .unwrap();
                assert_eq!(driver.calls, 1, "{boundary:?}");
                assert!(!driver.mutated);
                assert!(!driver.summary);
                exit
            };
            let output = retained.unwrap();
            assert_eq!(exit, output.plan.exit_code);
            assert_eq!(
                output
                    .error
                    .as_ref()
                    .unwrap()
                    .envelope(timestamp(), None)
                    .result()
                    .error_code(),
                Some("LongRunningOperationCancelled"),
            );
            let error = output
                .operation_error
                .as_ref()
                .expect("original operation error");
            assert!(!error.mutation_may_have_occurred);
            let mut expected = vec!["https://api.fabric.microsoft.com/v1/workspaces/w".to_owned()];
            match boundary {
                OperationBoundary::PrimaryCooperative => {
                    assert_eq!(error.kind, OperationErrorKind::Cancelled);
                    assert!(error.operation_id.is_none());
                }
                OperationBoundary::PrimaryExecution => {
                    assert_eq!(
                        error.kind,
                        OperationErrorKind::Execution(OperationExecutionError::Cancelled)
                    );
                    assert!(error.operation_id.is_none());
                }
                OperationBoundary::PolledTerminal | OperationBoundary::PolledExecution => {
                    assert_eq!(error.operation_id.as_deref(), Some(OPERATION_ID));
                    if matches!(boundary, OperationBoundary::PolledTerminal) {
                        assert!(matches!(
                            &error.kind,
                            OperationErrorKind::OperationFailed { status, .. } if status == "Cancelled"
                        ));
                        assert_eq!(
                            error.raw_service_error.as_deref(),
                            Some(&json!({"errorCode":"SyntheticCancellation","message":"Stopped"}))
                        );
                    } else {
                        assert_eq!(
                            error.kind,
                            OperationErrorKind::Execution(OperationExecutionError::Cancelled)
                        );
                    }
                    expected.push(format!(
                        "https://api.fabric.microsoft.com/v1/operations/{OPERATION_ID}"
                    ));
                }
            }
            assert_eq!(*operations.requests.lock().unwrap(), expected);
            assert_eq!(
                *host.http.requests.lock().unwrap(),
                ["https://api.fabric.microsoft.com/v1/workspaces"],
            );
            assert!(host.http.replies.lock().unwrap().is_empty());
            assert_eq!(host.state.path().unwrap(), "/Demo.Workspace");
        }
    }
}
