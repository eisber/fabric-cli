// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline executable observations through the production read session.
//! Synthetic credentials/HTTP live only in this development consumer.

#[allow(dead_code, unused_imports)]
mod support {
    include!("../tests/folder_support/mod.rs");
}

use fabric_cli::{
    ReadConfig,
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::Timestamp;
use serde_json::{Value, json};
use std::{
    future::poll_fn,
    io::{self, BufRead},
    sync::Arc,
    task::Poll,
};

fn main() {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line.expect("stdin")).expect("case JSON");
        let args = input["command"].as_array().expect("command argv");
        let Invocation::Read(command) =
            parse_args(std::iter::once("fab-rs").chain(args.iter().map(|v| v.as_str().unwrap())))
                .expect("read arguments")
        else {
            panic!("read required");
        };
        let credential = Arc::new(support::Credential::default());
        let sdk = support::sdk(credential.clone());
        let http = Arc::new(support::Http::new(
            serde_json::from_value::<Vec<support::Reply>>(
                input.get("responses").cloned().unwrap_or_else(|| json!([])),
            )
            .expect("synthetic responses"),
        ));
        let executor = support::executor(http.clone(), Arc::default());
        let session = ReadSession {
            sdk: &sdk,
            executor: &executor,
            tenant: Element::tenant("Offline tenant", "offline-tenant"),
            current_path: input["current"].as_str().unwrap_or("/"),
            home: input["home"]
                .as_str()
                .map_or(HomeContext::Unspecified, |s| {
                    HomeContext::Workspace(s.to_owned())
                }),
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig {
                folder_listing_enabled: input["folders"].as_bool().unwrap_or(false),
                ..ReadConfig::default()
            },
        };
        let cancelled = poll_fn(|_| {
            if input["cancelled"] == true {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let result = support::complete(session.dispatch(&command, cancelled));
        let (mut stdout, mut stderr) = (vec![], vec![]);
        let (exit, render_error) = match result {
            Ok(plan) => {
                plan.write_to(&mut stdout, &mut stderr).unwrap();
                (plan.exit_code, None)
            }
            Err(error) => {
                if let Some(partial) = error.partial_output() {
                    partial.write_to(&mut stdout, &mut stderr).unwrap();
                }
                (1, Some(error.to_string()))
            }
        };
        http.assert_consumed();
        println!(
            "{}",
            json!({
                "exit": exit,
                "stdout": String::from_utf8(stdout).unwrap(),
                "stderr": String::from_utf8(stderr).unwrap(),
                "requests": *http.requests.lock().unwrap(),
                "scopes": *credential.scopes.lock().unwrap(),
                "render_error": render_error,
            })
        );
    }
}
