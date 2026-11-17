// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! V2 query entrypoint reusing the actual root parser and existing NativeSession.

use super::{Failure, failed};
use fabric_cli::{
    native_tabular::Operation,
    parser::{Invocation, parse_args},
    query_host::native::{NativeQueryHost, QueryControl},
    tabular_export::FileSink,
};
use fabric_sdk::runtime::NativeDeadline;
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[path = "runtime_config.rs"]
#[allow(
    dead_code,
    reason = "This entrypoint consumes only the optional root from the shared path selector."
)]
mod config_root;
#[path = "query_binary_input.rs"]
mod input;

fn parse(args: &[OsString]) -> Result<Invocation, clap::Error> {
    parse_args(std::iter::once(OsString::from("fab-cli-v2")).chain(args.iter().cloned()))
}

pub(crate) fn claims(args: &[OsString]) -> bool {
    if args.first().map(OsString::as_os_str) != Some(OsStr::new("query")) {
        return false;
    }
    match parse(args) {
        Ok(Invocation::NativeTabular(command)) => matches!(command.operation, Operation::Query(_)),
        Err(_) => true,
        _ => false,
    }
}

pub(crate) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match parse(&args) {
        Ok(Invocation::NativeTabular(command))
            if matches!(command.operation, Operation::Query(_)) =>
        {
            command
        }
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            write!(io::stdout().lock(), "{error}")
                .map_err(|_| failed("OutputFailed", "Query help output failed.", 1))?;
            return Ok(0);
        }
        Err(error) => {
            return Err(failed(
                "InvalidArguments",
                fabric_cli::parser::invalid_arguments_message(&error),
                2,
            ));
        }
        _ => {
            return Err(failed(
                "InvalidArguments",
                "A root query invocation is required.",
                2,
            ));
        }
    };
    if let Operation::Query(options) = &command.operation
        && options.parameters.is_some()
    {
        return Err(failed(
            "UnsupportedParameters",
            "The native query providers do not support --parameters.",
            2,
        ));
    }
    let parsed = fabric_cli::query_host::resolution::parse_owner(&command.path)
        .map_err(|error| failed("InvalidTarget", error.to_string(), 2))?;
    let timeout = fabric_sdk_http::ExecutionOptions::default().timeout;
    let deadline = NativeDeadline::new(timeout)
        .map_err(|_| failed("InvalidConfiguration", "Invalid finite query deadline.", 2))?;
    let end = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| failed("InvalidConfiguration", "Invalid finite query deadline.", 2))?;
    let (selection, endpoint) = super::capture_fabric_selection()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failed("RuntimeFailed", "Query runtime could not start.", 1))?;
    let result=runtime.block_on(async {
        let settings=setup(|| {
            let root=config_root::available_root()
                .map_err(|_|failed("InvalidConfiguration","Invalid existing configuration root.",2))?;
            root.map(|root|fabric_cli_local::ConfigStore::new(root).read()
                .map_err(|_|failed("InvalidConfiguration","The existing query settings could not be read.",2)))
                .transpose()
        },end).await?;
        if matches!(&parsed,fabric_cli::get_target::GetTarget::Item {
            item_type:Some(kind),..
        } if kind=="DataAgent") {
            fabric_cli::query_host::connection::QueryConnection::from_settings(settings.as_ref(),&endpoint)
                .map_err(|error|failed("InvalidConfiguration",error.to_string(),2))?
                .ok_or_else(||failed("NotConfigured","DataAgent query requires an explicit trusted query_connection.",1))?;
        }
        let owned=command.clone();
        let mut input=setup(move ||input::read(&owned)
            .map_err(|error|failed("InvalidInput",error.to_string(),2)),end).await?;
        let session=super::NativeSession::new(selection,endpoint,timeout)?;
        let host=NativeQueryHost::new(
            fabric_sdk::Client::with_context(session.context.clone()),
            session.executor.as_ref().clone(),session.service_http.clone(),
            Arc::new(fabric_sdk_transport::TokioTimer),settings,
        );
        let mut random=[0u8;16];
        getrandom::fill(&mut random)
            .map_err(|_|failed("RuntimeFailed","Query correlation metadata could not be created.",1))?;
        let request_id=format!("query;{}",uuid::Uuid::from_bytes(random));
        let canceled=AtomicBool::new(false);
        let signal_failed=AtomicBool::new(false);
        let mut sink=FileSink::default();
        let observed=host.dispatch(&command,QueryControl {
            deadline:&deadline,end,canceled:&canceled,request_id:&request_id,
        },&mut input,&mut sink,async {
            if tokio::signal::ctrl_c().await.is_err() {
                signal_failed.store(true,Ordering::SeqCst);
            }
        }).await;
        match observed {
            Ok(mut delivery)=>{
                if signal_failed.load(Ordering::SeqCst) {
                    delivery.receipt.exit_code=1;
                    delivery.receipt.receipt["hostError"]=serde_json::json!({
                        "code":"SignalFailed",
                        "message":"Local interruption monitoring failed; execution evidence is retained without replay."
                    });
                }
                Ok(delivery)
            }
            Err(error)=>{
                if signal_failed.load(Ordering::SeqCst) {
                    Err(failed("SignalFailed","Local query interruption monitoring failed.",1))
                } else {
                    Err(failed("QueryFailed",error.to_string(),1))
                }
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    result?.write_to_streams(&mut io::stdout().lock(),&mut io::stderr().lock())
        .map_err(|error|{
            let receipt=&error.delivery.receipt.receipt;
            ("OutputFailed","Query output failed; retained execution/file evidence must be inspected before any further action.".into(),
                serde_json::json!({"execution":receipt["execution"]["status"],
                    "delivery":receipt["delivery"]["status"],"queryReplayed":false}),1)
        })
}

async fn setup<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
    end: Instant,
) -> Result<T, Failure> {
    tokio::select! {
        biased;
        signal=tokio::signal::ctrl_c()=>{
            match signal {
                Ok(())=>Err(failed("Cancelled","Query setup was canceled before query dispatch.",2)),
                Err(_)=>Err(failed("SignalFailed","Query interruption monitoring could not start.",1)),
            }
        }
        result=tokio::time::timeout_at(tokio::time::Instant::from_std(end),tokio::task::spawn_blocking(work))=>{
            result.map_err(|_|failed("TimedOut","Query setup deadline elapsed before query dispatch.",1))?
                .map_err(|_|failed("InputFailed","Query setup worker did not complete.",1))?
        }
    }
}
