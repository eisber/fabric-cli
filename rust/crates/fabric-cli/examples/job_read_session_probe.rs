// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline observations through the exact production parser and job read session.

#[allow(dead_code, unused_imports)]
mod support {
    include!("../tests/folder_support/mod.rs");
}

use fabric_cli::{
    ReadConfig, jobs::JobReadSession,
    parser::{Invocation, parse_args}, session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_jobs::HttpJobExecutor;
use fabric_cli_output::Timestamp;
use serde_json::{Value, json};
use std::{future::poll_fn, io::{self, BufRead}, sync::Arc, task::Poll};

fn main() {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line.expect("stdin")).expect("case JSON");
        let args = input["command"].as_array().expect("command argv");
        let Invocation::JobRead(command) =
            parse_args(std::iter::once("fab-rs").chain(args.iter().map(|v| v.as_str().unwrap())))
                .expect("job arguments")
        else { panic!("job read required") };
        let credential = Arc::new(support::Credential::default());
        let sdk = support::sdk(credential.clone());
        let mut backend = support::Http::new(
            serde_json::from_value::<Vec<support::Reply>>(input["responses"].clone()).unwrap(),
        );
        backend.hold_after = input["hold_after"].as_u64().map(|n| n as usize);
        let http = Arc::new(backend);
        let executor = support::executor(http.clone(), Arc::default());
        let reads = ReadSession {
            sdk: &sdk, executor: &executor,
            tenant: Element::tenant("Offline tenant", "offline-tenant"),
            current_path: input["current"].as_str().unwrap_or("/"),
            home: input["home"].as_str().map_or(HomeContext::Unspecified, |home| HomeContext::Workspace(home.into())),
            timestamp: Timestamp::parse(input["timestamp"].as_str().unwrap_or("2000-01-01T00:00:00Z")).unwrap(),
            config: ReadConfig::default(),
        };
        let session = JobReadSession {
            reads: &reads,
            jobs: Arc::new(HttpJobExecutor::new(executor.clone())),
        };
        let cancelled = poll_fn(|cx| {
            let held = http.hold_after.is_some_and(|n| http.requests.lock().unwrap().len() >= n);
            if input["cancelled"] == true || held {
                Poll::Ready(())
            } else {
                if http.hold_after.is_some() { cx.waker().wake_by_ref(); }
                Poll::Pending
            }
        });
        let result = support::complete(session.dispatch(&command, cancelled));
        let (mut stdout, mut stderr) = (vec![], vec![]);
        let (exit, render_error) = match result {
            Ok(plan) => { plan.write_to(&mut stdout, &mut stderr).unwrap(); (plan.exit_code, None) },
            Err(error) => {
                if let Some(partial) = error.partial_output() {
                    partial.write_to(&mut stdout, &mut stderr).unwrap();
                }
                (1, Some(error.to_string()))
            },
        };
        http.assert_consumed();
        assert!(http.headers.lock().unwrap().iter().all(|headers|
            headers.get_str(&"x-ms-fabric-skill".into()).is_err()
                && headers.get_str(&"x-ms-fabric-skill-version".into()).is_err()
        ));
        println!("{}", json!({
            "exit": exit, "stdout": String::from_utf8(stdout).unwrap(),
            "stderr": String::from_utf8(stderr).unwrap(),
            "requests": *http.requests.lock().unwrap(),
            "scopes": *credential.scopes.lock().unwrap(),
            "render_error": render_error,
        }));
    }
}
