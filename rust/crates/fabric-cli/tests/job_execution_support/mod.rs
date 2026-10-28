// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[allow(dead_code, unused_imports)]
mod reads {
    include!("../folder_support/mod.rs");
}

use async_trait::async_trait;
use azure_core::http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode, headers::Headers};
use fabric_cli::{
    ReadConfig,
    jobs::{JobSession, render_job_event},
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_jobs::{
    Confirmation, HttpJobExecutor, JobConfig, JobEvent, JobKind, JobUi, JsonInput,
};
use fabric_cli_output::{CommandError, Timestamp, text::TextPlan};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fmt,
    future::{pending, poll_fn},
    sync::{Arc, Mutex},
    task::Poll,
};

struct Http {
    replies: Mutex<VecDeque<reads::Reply>>,
    requests: Mutex<Vec<Value>>,
    hold_after: Option<usize>,
}

impl fmt::Debug for Http {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfflineJobHttp")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().scheme(), "https");
        assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
        assert!(request.headers().get_str(&"authorization".into()).is_ok());
        let body = match request.body() {
            Body::Bytes(bytes) if bytes.is_empty() => Value::Null,
            Body::Bytes(bytes) => serde_json::from_slice(bytes).unwrap(),
            _ => panic!("unexpected streaming request"),
        };
        let skill = request.headers().get_str(&"x-ms-fabric-skill".into()).ok();
        self.requests.lock().unwrap().push(json!({
            "method": request.method().to_string(), "url": request.url().as_str(),
            "body": body, "skill": skill,
        }));
        if self.hold_after == Some(self.requests.lock().unwrap().len()) {
            pending::<()>().await;
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP");
        let mut headers = Headers::new();
        for (key, value) in reply.headers {
            headers.insert(key, value);
        }
        let body = match reply.body {
            Value::String(raw) => raw.into_bytes(),
            value => serde_json::to_vec(&value).unwrap(),
        };
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::from(reply.status),
            headers,
            body,
        ))
    }
}

#[derive(Default)]
struct Ui {
    streams: Mutex<(Vec<u8>, Vec<u8>)>,
    events: Mutex<Vec<String>>,
    fail: bool,
    confirmed: bool,
    confirmations: Mutex<usize>,
    hold_confirmation: bool,
    fail_confirmation: bool,
    allow_confirmation: bool,
}

impl Ui {
    fn write(&self, plan: &TextPlan) {
        let mut streams = self.streams.lock().unwrap();
        let (stdout, stderr) = &mut *streams;
        plan.write_to(stdout, stderr).unwrap();
    }
}

impl JobUi for Ui {
    fn emit(&self, event: JobEvent) -> Result<(), CommandError> {
        self.events.lock().unwrap().push(format!("{event:?}"));
        if self.fail {
            return Err(CommandError::new(
                Some("Injected writer failure".into()),
                Some("UnexpectedError".into()),
            ));
        }
        let plan = render_job_event(&event).map_err(|error| {
            CommandError::new(Some(error.to_string()), Some("UnexpectedError".into()))
        })?;
        self.write(&plan);
        Ok(())
    }

    fn confirm(&self) -> Confirmation<'_> {
        assert!(self.allow_confirmation, "Only schedule removal can prompt");
        *self.confirmations.lock().unwrap() += 1;
        Box::pin(async {
            if self.hold_confirmation {
                pending::<()>().await;
            }
            if self.fail_confirmation {
                Err(CommandError::new(
                    Some("Injected confirmation failure".into()),
                    Some("UnexpectedError".into()),
                ))
            } else {
                Ok(self.confirmed)
            }
        })
    }
}

struct Files {
    values: Value,
    reads: Mutex<Vec<String>>,
}

impl JsonInput for Files {
    fn read_json(&self, path: &str) -> Result<Value, CommandError> {
        self.reads.lock().unwrap().push(path.into());
        self.values.get(path).cloned().ok_or_else(|| {
            CommandError::new(
                Some("Explicit fixture file not found".into()),
                Some("NotRunnable".into()),
            )
        })
    }
}

pub fn metadata(resource: &str) -> Vec<Value> {
    vec![
        json!({"status":200,"body":{"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}}),
        json!({"status":200,"body":{"value":[{"id":"n","displayName":"Book","type":resource}]}}),
    ]
}

pub fn accepted() -> Value {
    json!({"status":202,"body":null,"headers":{
        "Location":"https://api.fabric.microsoft.com/v1/workspaces/w/items/n/jobs/instances/run",
        "Retry-After":"10",
    }})
}

pub fn observe(input: &Value) -> Value {
    let args = input["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap());
    let command = match parse_args(std::iter::once("fab-rs").chain(args)).unwrap() {
        Invocation::JobRun(command) => command.family_command(),
        Invocation::JobMutation(command) => command.family_command(),
        Invocation::JobRead(command) => command.family_command(),
        _ => panic!("a job leaf is required"),
    };
    let credential = Arc::new(reads::Credential::default());
    let sdk = reads::sdk(credential.clone());
    let http = Arc::new(Http {
        replies: Mutex::new(serde_json::from_value(input["responses"].clone()).unwrap()),
        requests: Mutex::new(Vec::new()),
        hold_after: input["hold_after"].as_u64().map(|value| value as usize),
    });
    let timer = Arc::new(reads::Timer::default());
    let executor =
        HttpExecutor::new(http.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Offline tenant", "offline-tenant"),
        current_path: input["current"].as_str().unwrap_or("/"),
        home: input["home"]
            .as_str()
            .map_or(HomeContext::Unspecified, |home| {
                HomeContext::Workspace(home.into())
            }),
        timestamp: Timestamp::parse("2026-01-02T03:04:05.000006Z").unwrap(),
        config: ReadConfig::default(),
    };
    let files = Files {
        values: input["files"].clone(),
        reads: Mutex::new(Vec::new()),
    };
    let ui = Ui {
        fail: input["fail_ui"] == true,
        confirmed: input["confirmed"] == true,
        hold_confirmation: input["cancel_confirmation"] == true,
        fail_confirmation: input["fail_confirmation"] == true,
        allow_confirmation: command.kind == JobKind::Remove,
        ..Ui::default()
    };
    let session = JobSession {
        reads: &reads,
        jobs: Arc::new(HttpJobExecutor::new(executor.clone())),
        files: &files,
        ui: &ui,
        config: JobConfig {
            cancel_on_timeout: input["cancel_on_timeout"] != false,
        },
    };
    let cancelled = poll_fn(|cx| {
        if input["cancelled"] == true
            || (input["cancel_confirmation"] == true && *ui.confirmations.lock().unwrap() > 0)
            || http
                .hold_after
                .is_some_and(|count| http.requests.lock().unwrap().len() >= count)
        {
            Poll::Ready(())
        } else {
            if http.hold_after.is_some() || input["cancel_confirmation"] == true {
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        }
    });
    let plan = reads::complete(session.dispatch(&command, cancelled)).unwrap();
    ui.write(&plan);
    assert!(
        http.replies.lock().unwrap().is_empty(),
        "unused fixture replies"
    );
    let streams = ui.streams.lock().unwrap();
    let mut observation = json!({
        "exit":plan.exit_code,
        "stdout":String::from_utf8(streams.0.clone()).unwrap(),
        "stderr":String::from_utf8(streams.1.clone()).unwrap(),
        "requests":*http.requests.lock().unwrap(),
        "scopes":*credential.scopes.lock().unwrap(),
        "file_reads":*files.reads.lock().unwrap(),
        "waits":timer.0.lock().unwrap().iter().map(|delay| delay.as_secs()).filter(|delay| *delay != 240).collect::<Vec<_>>(),
    });
    if input["observe_confirmation"] == true {
        observation["confirmations"] = json!(*ui.confirmations.lock().unwrap());
    }
    observation
}
