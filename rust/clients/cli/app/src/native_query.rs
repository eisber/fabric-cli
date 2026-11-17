// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Query-only composition through the existing native factory and runtime.

use super::{CommandOutcome, failure};
use fabric_cli::{
    native_tabular::NativeCommand,
    query_host::native::{NativeQueryHost, QueryControl},
    tabular_export::FileSink,
};
use fabric_sdk::runtime::NativeDeadline;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[path = "query_binary_input.rs"]
mod input;

pub(super) fn run(command: &NativeCommand) -> io::Result<CommandOutcome> {
    if let fabric_cli::native_tabular::Operation::Query(options) = &command.operation
        && options.parameters.is_some()
    {
        return super::write_error(failure(
            "invalid_arguments",
            "The selected query providers do not support --parameters.",
            2,
        ));
    }
    let parsed = match fabric_cli::query_host::resolution::parse_owner(&command.path) {
        Ok(parsed) => parsed,
        Err(error) => {
            return super::write_error(failure("invalid_arguments", error.to_string(), 2));
        }
    };
    let timeout = fabric_sdk_http::ExecutionOptions::default().timeout;
    let deadline = NativeDeadline::new(timeout)
        .map_err(|_| io::Error::other("Invalid finite query deadline"))?;
    let end = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::other("Invalid finite query deadline"))?;
    let endpoint = match super::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC")) {
        Ok(endpoint) => endpoint,
        Err(error) => return super::write_error(error),
    };
    let credential = match super::selected_credential() {
        Ok(credential) => credential,
        Err(error) => return super::write_error(error),
    };
    let runtime = match super::native_runtime() {
        Ok(runtime) => runtime,
        Err(error) => return super::write_error(error),
    };
    let owned = command.clone();
    let result = runtime.block_on(async {
        let settings = super::execute::before_submission(async {
            tokio::task::spawn_blocking(|| {
                crate::local_host::available_store()
                    .and_then(|store| store.map(|store| store.read()).transpose())
                    .map_err(|_|failure("invalid_configuration","The existing query settings could not be read.",2))
            }).await.map_err(|_|failure("invalid_configuration","Query settings worker failed.",1))?
        },end).await?;
        if matches!(&parsed,fabric_cli::get_target::GetTarget::Item {
            item_type:Some(kind),..
        } if kind=="DataAgent") {
            fabric_cli::query_host::connection::QueryConnection::from_settings(settings.as_ref(),&endpoint)
                .map_err(|error|failure("invalid_configuration",error.to_string(),2))?
                .ok_or_else(||failure("NotConfigured","DataAgent query requires an explicit trusted query_connection.",1))?;
        }
        let mut input = super::execute::before_submission(async {
            tokio::task::spawn_blocking(move || input::read(&owned)
                .map_err(|error| failure("invalid_arguments", error.to_string(), 2)))
                .await.map_err(|_| failure("input_failed","Query input worker failed before dispatch.",1))?
        },end).await?;
        let (http,identity_http) = super::native_clients(&credential,true)?;
        let (sdk,metadata) = super::connect_native(
            credential,http.clone(),identity_http,endpoint,
            fabric_sdk_http::ExecutionOptions::default(),Some(end),
        ).await?;
        let host = NativeQueryHost::new(sdk,metadata,http,
            Arc::new(fabric_sdk_transport::TokioTimer),settings);
        let mut random=[0u8;16];
        getrandom::fill(&mut random)
            .map_err(|_|failure("runtime_failed","Query correlation metadata could not be created.",1))?;
        let request_id=format!("query;{}",uuid::Uuid::from_bytes(random));
        let canceled=AtomicBool::new(false);
        let signal_failed=AtomicBool::new(false);
        let mut sink=FileSink::default();
        let result=host.dispatch(command,QueryControl {
            deadline:&deadline,end,canceled:&canceled,request_id:&request_id,
        },&mut input,&mut sink,async {
            if tokio::signal::ctrl_c().await.is_err() {
                signal_failed.store(true,Ordering::SeqCst);
            }
        }).await;
        match result {
            Ok(mut delivery) => {
                if signal_failed.load(Ordering::SeqCst) {
                    delivery.receipt.exit_code=1;
                    delivery.receipt.receipt["hostError"]=serde_json::json!({
                        "code":"signal_failed",
                        "message":"Local interruption monitoring failed; execution evidence is retained and the query was not replayed."
                    });
                }
                Ok(delivery)
            }
            Err(error) => {
                if signal_failed.load(Ordering::SeqCst) {
                    Err(failure("signal_failed","Local query interruption monitoring failed.",1))
                } else {
                    Err(failure("query_failed",error.to_string(),1))
                }
            }
        }
    });
    match super::finish_runtime(runtime, result) {
        Ok(delivery) => {
            let exit = delivery
                .write_to_streams(&mut io::stdout().lock(), &mut io::stderr().lock())
                .map_err(|error| error.into_io_error())?;
            Ok(if exit == 0 {
                CommandOutcome::Returned(0)
            } else {
                CommandOutcome::Stop(exit)
            })
        }
        Err(error) => super::write_error(error),
    }
}
