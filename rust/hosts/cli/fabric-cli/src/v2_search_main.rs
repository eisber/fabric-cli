// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Search composition shares the explicit v2 identity, runtime and HTTP policies.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::search_command::{
    SearchArgumentError, SearchDelivery, SearchDeliveryError, parse_search, sdk_request,
};
use fabric_sdk::{
    EndpointBinding, ServiceOrigin,
    search::{
        CatalogProvider, Guid, RelevanceProvider, SearchBridgeError, SearchError, SearchMode,
        SearchOperation, SearchRequest, execute_approved_search, prepare_search, schemas,
    },
    v2::{ApprovalPolicy, Availability, Effect, Outcome, Precondition, PreparedOperation},
};
use fabric_sdk_api::ApiClient;
use fabric_sdk_catalog::CatalogClient;
use fabric_sdk_relevance::RelevanceClient;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, Write},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) fn run(args: Vec<OsString>) -> Result<(), Failure> {
    let command = match parse_search(args) {
        Ok(command) => command,
        Err(SearchArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)) => {
            fabric_cli::search_command::search_command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Search help output failed.", 1))?;
            return Ok(());
        }
        Err(error) => return Err(failed("InvalidArguments", error.to_string(), 2)),
    };
    if command.schema {
        let mut bytes = serde_json::to_vec(&schemas()).map_err(|_| {
            failed(
                "SerializationFailed",
                "Search schema could not be encoded.",
                1,
            )
        })?;
        bytes.push(b'\n');
        if bytes.len() > command.max_bytes.get() {
            return Err(failed(
                "BudgetExceeded",
                "Search schema exceeds the output bound.",
                1,
            ));
        }
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&bytes)
            .and_then(|_| stdout.flush())
            .map_err(|_| failed("OutputFailed", "Search schema output failed.", 1))?;
        return Ok(());
    }

    let started = Instant::now();
    let timeout = command.timeout;
    fabric_sdk::search::TypeFilters::new(command.types.clone(), command.exclude_types.clone())
        .map_err(search_failure)?;
    let cwd = std::env::current_dir().map_err(|_| {
        failed(
            "InvalidWorkingDirectory",
            "Output working directory unavailable.",
            2,
        )
    })?;
    let delivery = SearchDelivery::preflight(&command, &cwd)
        .map_err(|error| failed("OutputFailed", error.to_string(), 1))?;
    let relevance_endpoint = if command.mode != SearchMode::Catalog {
        let host = std::env::var("FAB_RUST_RELEVANCE_HOST").map_err(|_| {
            failed("NotConfigured", "Select an approved explicit FAB_RUST_RELEVANCE_HOST; no relevance authority is inferred.", 1)
        })?;
        if std::env::var("FAB_RUST_RELEVANCE_AUDIENCE").as_deref() != Ok("fabric") {
            return Err(failed(
                "NotConfigured",
                "Explicit FAB_RUST_RELEVANCE_AUDIENCE=fabric is required for this binding; no alternate token resource is inferred.",
                1,
            ));
        }
        Some(
            EndpointBinding::with_host(ServiceOrigin::RelevanceEngine, &host).map_err(|_| {
                failed(
                    "InvalidConfiguration",
                    "Invalid explicit relevance endpoint.",
                    2,
                )
            })?,
        )
    } else {
        None
    };
    let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
        failed(
            "NotConfigured",
            "Select an explicit FAB_RUST_AUTH_MODE; no credential chain is used.",
            1,
        )
    })?;
    let selection = headless::Selection::capture(&mode)
        .map_err(|(code, message, exit)| failed(code, message, exit))?;
    let request = sdk_request(&command, &selection.tenant).map_err(search_failure)?;
    let canonical_path = request
        .scope()
        .workspace_id()
        .map(|workspace| format!("/workspaces/{}", workspace.as_str()))
        .unwrap_or_else(|| "/".into());
    let mut prepared = prepare_search(&request, &canonical_path).map_err(bridge_failure)?;
    let output_path = delivery
        .output_path()
        .map(|path| {
            path.to_str().map(str::to_owned).ok_or_else(|| {
                failed(
                    "InvalidArguments",
                    "Output path cannot be represented in approval evidence.",
                    2,
                )
            })
        })
        .transpose()?;
    if let Some(path) = &output_path {
        let mut effects = Vec::new();
        if command.mode != SearchMode::Catalog {
            effects.push(Effect::ModelExecution);
        }
        effects.push(Effect::LocalWrite);
        prepared = prepared
            .with_optional_effects(effects)
            .and_then(|prepared| {
                prepared.with_preconditions(vec![Precondition::LocalOutput { path: path.clone() }])
            })
            .map_err(|_| {
                failed(
                    "InvalidArguments",
                    "Invalid search output effect preparation.",
                    2,
                )
            })?;
    }
    let approved = prepared
        .authorize(&mut SearchPolicy {
            request: &request,
            path: &canonical_path,
            output_path: output_path.as_deref(),
        })
        .map_err(|message| failed("ApprovalDenied", message, 1))?;

    let endpoint = match std::env::var("FAB_RUST_FABRIC_HOST") {
        Ok(host) => EndpointBinding::with_host(ServiceOrigin::Fabric, &host).map_err(|_| {
            failed(
                "InvalidConfiguration",
                "Invalid explicit Fabric endpoint.",
                2,
            )
        })?,
        Err(std::env::VarError::NotPresent) => EndpointBinding::public(ServiceOrigin::Fabric),
        Err(_) => {
            return Err(failed(
                "InvalidConfiguration",
                "Invalid explicit Fabric endpoint.",
                2,
            ));
        }
    };
    if endpoint.host() != ServiceOrigin::Fabric.host() {
        return Err(failed(
            "UnsupportedCapability",
            "Search preview supports the explicit public service bindings only.",
            2,
        ));
    }
    let tenant = selection.tenant.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Search runtime could not be created.", 1))?;
    let result = runtime.block_on(async {
        let session = NativeSession::new(selection, endpoint, timeout)?;
        let api = ApiClient::new(
            session.context.credential().clone(),
            session.context.selection().clone(),
            Arc::new(fabric_cli_resources::ResourceHttpExecutor(
                session.executor.clone(),
            )),
        );
        let catalog = if command.mode != SearchMode::Relevance {
            Some(CatalogProvider::new(
                CatalogClient::new(api.clone()),
                Guid::new(&tenant).map_err(search_failure)?,
            ))
        } else {
            None
        };
        let relevance = if command.mode != SearchMode::Catalog {
            let endpoint = relevance_endpoint
                .ok_or_else(|| failed("NotConfigured", "Relevance binding is unavailable.", 1))?;
            let relevance_api = ApiClient::new(
                session.context.credential().clone(),
                session.context.selection().clone(),
                Arc::new(fabric_cli_resources::ResourceHttpExecutor(
                    session.executor_for(endpoint.clone(), timeout)?,
                )),
            );
            let client = RelevanceClient::from_api(relevance_api, endpoint).map_err(|_| {
                failed(
                    "InvalidConfiguration",
                    "Invalid explicit relevance binding.",
                    2,
                )
            })?;
            Some(RelevanceProvider::new(
                client,
                Guid::new(&tenant).map_err(search_failure)?,
            ))
        } else {
            None
        };
        let operation = SearchOperation::new(
            catalog
                .as_ref()
                .map(|provider| provider as &dyn fabric_sdk::search::SearchProvider),
            relevance
                .as_ref()
                .map(|provider| provider as &dyn fabric_sdk::search::SearchProvider),
        );
        let remaining = timeout.saturating_sub(started.elapsed());
        let cancelled = async {
            tokio::select! {
                _ = tokio::time::sleep(remaining) => (),
                _ = tokio::signal::ctrl_c() => (),
            }
        };
        execute_approved_search(&approved, &request, &operation, cancelled)
            .await
            .map_err(bridge_failure)
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let receipt = result.map_err(|failure| {
        if failure.0 == "Cancelled" && started.elapsed() >= timeout {
            failed(
                "TimedOut",
                "The total search deadline elapsed; remote cancellation is not implied.",
                1,
            )
        } else {
            failure
        }
    })?;
    if started.elapsed() >= timeout {
        return Err(failed(
            "TimedOut",
            "The total search deadline elapsed before delivery.",
            1,
        ));
    }
    let succeeded = matches!(receipt.outcome(), Outcome::Succeeded { .. });
    let envelope = serde_json::to_value(&receipt).map_err(|_| {
        failed(
            "SerializationFailed",
            "Search receipt could not be encoded.",
            1,
        )
    })?;
    delivery
        .deliver(envelope, &mut io::stdout().lock())
        .map_err(|error| {
            let local_output = match &error {
                SearchDeliveryError::File(failure) => json!({
                    "pathCreated":failure.path_created,
                    "bytesWritten":failure.bytes_written,
                    "rollback":false
                }),
                _ => Value::Null,
            };
            (
                "OutputFailed",
                error.to_string(),
                json!({
                    "execution":receipt.evidence(),
                    "sources":receipt.sources(),
                    "target":receipt.target(),
                    "localOutput":local_output,
                    "remoteReplayed":false
                }),
                1,
            )
        })?;
    if succeeded {
        Ok(())
    } else {
        Err(failed(
            "IncompleteSearch",
            "Search was partial or failed; inspect the retained source evidence.",
            1,
        ))
    }
}

fn bridge_failure(error: SearchBridgeError) -> Failure {
    let evidence = match &error {
        SearchBridgeError::OutputBudget { receipt } => json!({
            "execution":receipt.evidence(), "sources":receipt.sources(), "target":receipt.target(),
            "remoteReplayed":false
        }),
        SearchBridgeError::ReceiptBuild { failure, sources } => json!({
            "execution":failure.evidence, "sources":sources, "remoteReplayed":false
        }),
        SearchBridgeError::Sources(failure) => json!({
            "execution":failure.receipt.evidence(), "sources":failure.sources, "remoteReplayed":false
        }),
        _ => Value::Null,
    };
    ("SearchFailed", error.to_string(), evidence, 1)
}

fn search_failure(error: SearchError) -> Failure {
    let evidence = match &error {
        SearchError::AllSourcesFailed { sources } => json!({"sources":sources}),
        _ => Value::Null,
    };
    let code = match &error {
        SearchError::InvalidInput { .. } => "InvalidArguments",
        SearchError::UnsupportedCapability { .. } => "UnsupportedCapability",
        SearchError::MissingProvider { .. } => "NotConfigured",
        SearchError::Cancelled => "Cancelled",
        _ => "SearchFailed",
    };
    let exit = if matches!(
        error,
        SearchError::InvalidInput { .. } | SearchError::UnsupportedCapability { .. }
    ) {
        2
    } else {
        1
    };
    (code, error.to_string(), evidence, exit)
}

struct SearchPolicy<'a> {
    request: &'a SearchRequest,
    path: &'a str,
    output_path: Option<&'a str>,
}

impl ApprovalPolicy for SearchPolicy<'_> {
    type Error = &'static str;

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        let input = serde_json::to_value(self.request).map_err(|_| "Invalid search input.")?;
        let mut effects = vec![Effect::RemoteRead];
        if self.request.mode() != SearchMode::Catalog {
            effects.push(Effect::ModelExecution);
        }
        if self.output_path.is_some() {
            effects.push(Effect::LocalWrite);
        }
        let output_matches = match self.output_path {
            Some(path) => {
                operation.preconditions() == [Precondition::LocalOutput { path: path.into() }]
            }
            None => operation.preconditions().is_empty(),
        };
        if operation.operation_id() != "catalog.search"
            || operation.input() != &input
            || operation.target().canonical_path() != self.path
            || operation.effects().len() != effects.len()
            || effects
                .iter()
                .any(|effect| !operation.effects().contains(effect))
            || !output_matches
            || !matches!(operation.availability(), Availability::Available)
        {
            return Err(
                "The exact search scope, input, effects or prerequisites were not approved.",
            );
        }
        Ok(())
    }
}
