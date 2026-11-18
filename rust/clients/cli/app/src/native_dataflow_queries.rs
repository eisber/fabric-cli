// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata-only Dataflow discovery over the existing selected native host.

use super::{BatchState, CommandOutcome, failure};
use fabric_cli::execute::{ListQueriesCommand, QueryTarget};
use fabric_sdk::{
    Client,
    dataflow_queries::{QueryDiscoveryError, QueryInventory, list_queries},
    representations::{DefinitionTarget, RepresentationLimits},
};
use fabric_sdk_operations::OperationExecutor;
use std::{future::Future, io, num::NonZeroUsize, time::Instant};

pub(super) fn run(
    command: &ListQueriesCommand,
    state: &mut BatchState,
) -> io::Result<CommandOutcome> {
    let deadline = Instant::now()
        .checked_add(command.timeout)
        .ok_or_else(|| io::Error::other("Invalid discovery deadline"))?;
    let runtime = super::native_runtime().map_err(|(_, message, _)| io::Error::other(message))?;
    let outcome = runtime.block_on(async {
        let result = async {
            use std::io::IsTerminal;
            if cfg!(windows) && io::stdout().is_terminal() {
                return Err(failure(
                    "UnsupportedCapability",
                    "Redirect stdout to a file or pipe to preserve query-name Unicode. No definition was requested.",
                    1,
                ));
            }
            let endpoint = super::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
            let credential = super::selected_credential()?;
            credential.bind_get(state).map_err(|_| {
                failure("invalid_configuration", "Discovery identity binding changed.", 2)
            })?;
            let (http, identity_http) = super::native_clients(&credential, false)?;
            let (sdk, executor) = super::connect_native(
                credential,
                http,
                identity_http,
                endpoint,
                fabric_sdk_http::ExecutionOptions::default(),
                Some(deadline),
            )
            .await?;
            let cancelled = async {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = tokio::time::sleep_until(deadline.into()) => {},
                }
            };
            let inventory = super::execute::before_submission(
                discover(command, &sdk, &executor, cancelled),
                deadline,
            )
            .await?;
            let bytes = render(command, inventory)?;
            let output =
                super::output::owned_stream(fabric_cli_output::OutputStream::Stdout, false)
                    .map_err(|_| {
                        failure("output_failed", "Unable to retain discovery stdout.", 1)
                    })?;
            let cancelled = async {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => failure("cancelled", "Discovery output waiting was interrupted.", 2),
                    Err(_) => failure("signal_failed", "Discovery output cancellation handler failed.", 1),
                }
            };
            match super::output::write_owned(output, deadline, cancelled, move |output| {
                use std::io::Write;
                output.write_all(&bytes)?;
                output.flush()
            })
            .await
            {
                super::output::OwnedWrite::Complete(Ok(())) => Ok(()),
                _ => Err(failure(
                    "output_failed",
                    "Discovery output is unconfirmed and may be partial. No query was evaluated.",
                    1,
                )),
            }
        }.await;
        match result {
            Ok(()) => CommandOutcome::Returned(0),
            Err(error) => {
                let exit = error.2;
                match super::output::owned_stream(fabric_cli_output::OutputStream::Stderr, false) {
                    Ok(output) => diagnostic_to(error, output, deadline).await,
                    Err(_) => CommandOutcome::Stop(exit),
                }
            }
        }
    });
    Ok(super::finish_runtime(runtime, outcome))
}

async fn diagnostic_to(
    (code, message, exit): super::Failure,
    output: impl std::io::Write + Send + 'static,
    deadline: Instant,
) -> CommandOutcome {
    let mut bytes =
        serde_json::to_vec(&serde_json::json!({"error":{"code":code,"message":message}}))
            .expect("bounded diagnostic values serialize");
    bytes.push(b'\n');
    let deadline = deadline.min(Instant::now() + std::time::Duration::from_secs(1));
    let delivery = super::output::write_owned(
        output,
        deadline,
        std::future::pending::<()>(),
        move |output| {
            output.write_all(&bytes)?;
            output.flush()
        },
    )
    .await;
    match delivery {
        super::output::OwnedWrite::Complete(Ok(())) => CommandOutcome::Stop(exit),
        // Diagnostic delivery is unconfirmed. Never synchronously retry a full
        // stderr/combined pipe, or continue a batch while an OS write can finish.
        _ => CommandOutcome::Stop(exit.max(1)),
    }
}

pub(super) async fn discover(
    command: &ListQueriesCommand,
    sdk: &Client,
    executor: &dyn OperationExecutor,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<QueryInventory, super::Failure> {
    let cap = NonZeroUsize::new(command.max_bytes)
        .ok_or_else(|| failure("invalid_arguments", "Invalid discovery byte limit.", 2))?;
    list_queries(
        sdk,
        executor,
        &DefinitionTarget {
            workspace_id: command.target.workspace_id().to_owned(),
            item_id: command.target.dataflow_id().to_owned(),
            native_format: None,
        },
        RepresentationLimits {
            max_response_bytes: cap,
            max_result_bytes: cap,
        },
        cancelled,
    )
    .await
    .map_err(|error| {
        let message = match error {
            QueryDiscoveryError::InvalidMetadata => {
                "Dataflow query metadata is unsupported, malformed, ambiguous or exceeds its bounds."
            }
            _ => "Dataflow definition retrieval failed or was interrupted. No query was evaluated.",
        };
        failure("dataflow_discovery_failed", message, 1)
    })
}

pub(super) fn render(
    command: &ListQueriesCommand,
    inventory: QueryInventory,
) -> Result<Vec<u8>, super::Failure> {
    let queries: Vec<_> = inventory
        .queries
        .into_iter()
        .map(|query| {
            let path = format!(
                "/{}.Workspace/{}.Dataflow/queries/{}",
                command.target.workspace_id(),
                command.target.dataflow_id(),
                query.query_name
            );
            let target = QueryTarget::parse(&path).ok().map(|_| path);
            let addressable = target.is_some();
            serde_json::json!({
                "queryId": query.query_id,
                "queryName": query.query_name,
                "isHidden": query.is_hidden,
                "loadEnabled": query.load_enabled,
                "executePath": target,
                "addressable": addressable,
            })
        })
        .collect();
    let mut bytes = serde_json::to_vec(&serde_json::json!({
        "queries": queries,
        "queryMetadataPresent": inventory.query_metadata_present,
        "operationId": inventory.operation_id,
        "queryExecuted": false,
    }))
    .map_err(|_| failure("output_failed", "Unable to encode query metadata.", 1))?;
    bytes.push(b'\n');
    if bytes.len() > command.max_bytes {
        return Err(failure(
            "result_limit",
            "Complete query metadata exceeds the byte limit.",
            1,
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_sdk::dataflow_queries::NamedQuery;

    fn command() -> ListQueriesCommand {
        let fabric_cli::parser::Invocation::DataflowQueries(command) =
            fabric_cli::parser::parse_args([
                "fab-rs", "execute",
                "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Dataflow",
                "--list-queries",
            ]).unwrap()
        else { panic!("discovery command") };
        command
    }

    #[test]
    fn discovery_render_preserves_names_and_marks_unaddressable_queries_without_guessing() {
        let inventory = QueryInventory {
            query_metadata_present: true,
            operation_id: Some("actual-operation".into()),
            queries: ["Orders", "雪 with spaces", "A/B", r"A\B", "%escaped", ".."]
                .into_iter()
                .map(|name| NamedQuery {
                    query_id: "native-id".into(),
                    query_name: name.into(),
                    is_hidden: true,
                    load_enabled: false,
                })
                .collect(),
        };
        let bytes = render(&command(), inventory).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["operationId"], "actual-operation");
        assert_eq!(value["queryExecuted"], false);
        assert_eq!(value["queries"][1]["queryName"], "雪 with spaces");
        for (index, query) in value["queries"].as_array().unwrap().iter().enumerate() {
            assert_eq!(query["addressable"], index < 2);
            assert_eq!(query["executePath"].is_null(), index >= 2);
            assert_eq!(query["isHidden"], true);
            assert_eq!(query["loadEnabled"], false);
        }
    }

    #[test]
    fn absent_query_index_is_explicit_and_complete_output_never_clips() {
        let inventory = QueryInventory {
            query_metadata_present: false,
            operation_id: None,
            queries: Vec::new(),
        };
        let bytes = render(&command(), inventory.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["queryMetadataPresent"], false);
        assert_eq!(value["queries"], serde_json::json!([]));
        let mut command = command();
        command.max_bytes = bytes.len() - 1;
        assert_eq!(
            render(&command, inventory.clone()).unwrap_err().0,
            "result_limit"
        );
        command.max_bytes = bytes.len();
        assert_eq!(render(&command, inventory).unwrap(), bytes);
    }

    struct CountWrites(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl io::Write for CountWrites {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn expired_discovery_deadline_never_starts_an_error_write_or_continues_batch() {
        let writes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let outcome = diagnostic_to(
            failure("timed_out", "Complete output is unconfirmed.", 1),
            CountWrites(writes.clone()),
            Instant::now() - std::time::Duration::from_millis(1),
        )
        .await;
        assert!(matches!(outcome, CommandOutcome::Stop(1)));
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    struct BlockWrites {
        gate: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl io::Write for BlockWrites {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (gate, changed) = &*self.gate;
            let mut released = gate.lock().unwrap();
            while !*released {
                released = changed.wait(released).unwrap();
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn blocked_diagnostic_pipe_returns_stop_without_synchronous_retry() {
        let gate = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let writes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let result = diagnostic_to(
            failure("output_failed", "Output is unconfirmed.", 1),
            BlockWrites {
                gate: gate.clone(),
                writes: writes.clone(),
            },
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .await;
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(matches!(result, CommandOutcome::Stop(1)));
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
