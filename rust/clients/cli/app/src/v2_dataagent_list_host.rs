// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One explicit DataAgent datasource collection page, shared by root and facade.

use crate::{
    get_budget::GetReadBudget,
    get_resolution::{GetResolutionError, ResolvedGetTarget, resolve},
    get_target::{GetRepresentation, GetTarget},
};
use clap::{Arg, Command, error::ErrorKind};
use fabric_cli_storage_files::{ExactOutputError, ExactOutputFile};
use fabric_sdk::{
    Client,
    dataagents::{ContinuationToken, DataAgentError, DataAgentRequests, DataAgentTarget, Stage},
    items::ItemExecutor,
    representations::RepresentationLimits,
    workspaces::WorkspaceExecutor,
};
use fabric_sdk_operations::{OperationErrorKind, OperationExecutor};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::{Future, pending, poll_fn},
    io::Write,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::pin,
    task::Poll,
    time::Duration,
};

const RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// Validated invocation; construction is restricted to the shared parser.
pub struct ListCommand {
    path: String,
    owner: GetTarget,
    stage: Stage,
    continuation: Option<ContinuationToken>,
    limit: NonZeroUsize,
    max_bytes: NonZeroUsize,
    timeout: Duration,
    project: Option<String>,
    output: Option<PathBuf>,
}

impl ListCommand {
    /// One total host deadline, including identity setup and output.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// Sanitized parser failure; supplied paths, tokens and expressions are omitted.
#[derive(Debug, thiserror::Error)]
#[error("Invalid datasource list arguments; run fab-cli-v2 list --help.")]
pub enum ListArgumentError {
    /// Clap classification, including effect-free help.
    Syntax(ErrorKind),
    /// Unsupported collection, mismatched stage, expression or safety bound.
    InvalidInput,
}

/// Local interruption never requests service cancellation or rollback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListInterruption {
    /// The caller stopped local work.
    Cancelled,
    /// The original total deadline elapsed.
    TimedOut,
}

/// Original typed failures remain available without formatting service content.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    /// Complete records cannot fit the selected bounds; nothing was skipped.
    #[error(
        "The native page exceeds the selected record or data-byte budget; increase the finite bound."
    )]
    BudgetExceeded,
    /// Local work did not complete.
    #[error(
        "Datasource listing was interrupted; no remote cancellation or rollback was requested."
    )]
    Interrupted(ListInterruption),
    /// Blocking output may already have written bytes; no rollback is implied.
    #[error(
        "Datasource output was interrupted; delivery may be partial or complete and was not rolled back."
    )]
    DeliveryInterrupted(ListInterruption),
    /// Exact typed owner resolution did not complete.
    #[error("Bounded DataAgent owner resolution failed.")]
    Resolution(#[source] Box<GetResolutionError>),
    /// Original native read failure, never an empty page or another stage.
    #[error("The native DataAgent datasource read failed or is incomplete.")]
    Read(#[source] Box<DataAgentError>),
    /// Data-only JMESPath evaluation failed.
    #[error("Datasource data projection failed.")]
    Projection(#[source] fabric_cli_query::QueryError),
    /// Exclusive local output failed, retaining possible partial-write evidence.
    #[error("Datasource output file preflight or delivery failed.")]
    File(#[source] Box<ExactOutputError>),
    /// Serialization, stream write or flush failed.
    #[error("Datasource output could not be completed.")]
    Output,
}

impl ListError {
    /// Stable sanitized classification; callers may inspect original typed evidence.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BudgetExceeded => "BudgetExceeded",
            Self::Interrupted(ListInterruption::Cancelled)
            | Self::DeliveryInterrupted(ListInterruption::Cancelled) => "Cancelled",
            Self::Interrupted(ListInterruption::TimedOut)
            | Self::DeliveryInterrupted(ListInterruption::TimedOut) => "TimedOut",
            Self::Resolution(_) => "TargetResolutionFailed",
            Self::Read(error) => {
                let operation = match error.as_ref() {
                    DataAgentError::Operation(error) => Some(error),
                    DataAgentError::Response(error) => Some(&error.error),
                    _ => None,
                };
                match operation.map(|error| &error.kind) {
                    Some(OperationErrorKind::Http {
                        status: 401 | 403, ..
                    }) => "PermissionDenied",
                    Some(OperationErrorKind::Limit) => "BudgetExceeded",
                    Some(OperationErrorKind::Cancelled) => "Cancelled",
                    _ => "DataAgentReadFailed",
                }
            }
            Self::Projection(_) => "ProjectionFailed",
            Self::File(_) | Self::Output => "OutputFailed",
        }
    }
}

/// Recognize only the owned root command or exact DataAgent list facade.
pub fn is_list(args: &[OsString]) -> bool {
    args.first().and_then(|value| value.to_str()) == Some("list")
        || args.get(..3).is_some_and(|prefix| {
            prefix.iter().map(|value| value.to_str()).eq([
                Some("artifact"),
                Some("data-agent"),
                Some("list"),
            ])
        })
}

/// Build the same finite option schema for either spelling.
pub fn list_command() -> Command {
    Command::new("list")
        .about("Read one explicit DataAgent datasource collection page")
        .args_override_self(false)
        .infer_long_args(false)
        .after_help("Collections: /<workspace>.Workspace/<agent>.DataAgent/settings/staging/datasources --stage draft, or settings/published/datasources --stage published. Stage is required and must match the path. Only one native datasource page is read; continuation URIs are inert data and never followed. Incomplete collections exit nonzero. No --all, mutation, model, query, source creation or stage fallback.")
        .arg(Arg::new("path").required(true).num_args(1))
        .arg(Arg::new("stage").long("stage").required(true).num_args(1).value_parser(["draft", "published"]))
        .arg(Arg::new("continuation").long("continuation").num_args(1).value_name("OPAQUE_TOKEN"))
        .arg(Arg::new("limit").long("limit").num_args(1).default_value("100").value_parser(clap::value_parser!(NonZeroUsize)))
        .arg(Arg::new("max-bytes").long("max-bytes").num_args(1).default_value("1048576").value_parser(clap::value_parser!(NonZeroUsize)))
        .arg(Arg::new("timeout").long("timeout").num_args(1).default_value("30s").value_parser(parse_duration))
        .arg(Arg::new("format").long("format").num_args(1).default_value("json").value_parser(["json"]))
        .arg(Arg::new("project").long("project").num_args(1))
        .arg(Arg::new("output").long("output").num_args(1).value_parser(clap::builder::OsStringValueParser::new()))
}

/// Parse without filesystem, credential, transport or environment access.
///
/// # Errors
/// Rejects all noncollection targets, implicit/mismatched stages and unknown flags.
pub fn parse_list(args: Vec<OsString>) -> Result<ListCommand, ListArgumentError> {
    if !is_list(&args) {
        return Err(ListArgumentError::InvalidInput);
    }
    let root = Command::new("fab-cli-v2")
        .subcommand(list_command())
        .subcommand(
            Command::new("artifact")
                .subcommand(Command::new("data-agent").subcommand(list_command())),
        );
    let matches = root
        .try_get_matches_from(std::iter::once(OsString::from("fab-cli-v2")).chain(args))
        .map_err(|error| ListArgumentError::Syntax(error.kind()))?;
    let matches = if let Some(matches) = matches.subcommand_matches("list") {
        matches
    } else {
        matches
            .subcommand_matches("artifact")
            .and_then(|matches| matches.subcommand_matches("data-agent"))
            .and_then(|matches| matches.subcommand_matches("list"))
            .ok_or(ListArgumentError::InvalidInput)?
    };
    let path = matches.get_one::<String>("path").expect("required path");
    if path.len() > 16_384
        || path.chars().any(char::is_control)
        || path.contains(['\\', '*', '?', '[', ']', '{', '}', '%', '#'])
        || path.split('/').any(|part| part.trim() != part)
    {
        return Err(ListArgumentError::InvalidInput);
    }
    let stage = match matches.get_one::<String>("stage").map(String::as_str) {
        Some("draft") => Stage::Staging,
        Some("published") => Stage::Published,
        _ => return Err(ListArgumentError::InvalidInput),
    };
    let suffix = match stage {
        Stage::Staging => "/settings/staging/datasources",
        Stage::Published => "/settings/published/datasources",
    };
    let owner_path = path
        .strip_suffix(suffix)
        .ok_or(ListArgumentError::InvalidInput)?;
    let owner = GetTarget::parse(owner_path).map_err(|_| ListArgumentError::InvalidInput)?;
    if !matches!(&owner, GetTarget::Item {
        item_type: Some(kind), representation: GetRepresentation::Metadata, ..
    } if kind == "DataAgent")
    {
        return Err(ListArgumentError::InvalidInput);
    }
    let continuation = matches
        .get_one::<String>("continuation")
        .map(|token| {
            if token.len() > 16_384 {
                return Err(ListArgumentError::InvalidInput);
            }
            ContinuationToken::new(token.clone()).map_err(|_| ListArgumentError::InvalidInput)
        })
        .transpose()?;
    let project = matches.get_one::<String>("project").cloned();
    if let Some(query) = &project {
        fabric_cli_query::validate_projection(query)
            .map_err(|_| ListArgumentError::InvalidInput)?;
    }
    let max_bytes = *matches
        .get_one::<NonZeroUsize>("max-bytes")
        .expect("default bound");
    if max_bytes.get() > RESPONSE_BYTES {
        return Err(ListArgumentError::InvalidInput);
    }
    Ok(ListCommand {
        path: path.clone(),
        owner,
        stage,
        continuation,
        limit: *matches
            .get_one::<NonZeroUsize>("limit")
            .expect("default limit"),
        max_bytes,
        timeout: *matches
            .get_one::<Duration>("timeout")
            .expect("default timeout"),
        project,
        output: matches
            .get_one::<OsString>("output")
            .filter(|path| path.as_os_str() != "-")
            .map(PathBuf::from),
    })
}

fn parse_duration(value: &str) -> Result<Duration, &'static str> {
    let (number, unit) = ["ms", "s", "m", "h"]
        .into_iter()
        .find_map(|unit| value.strip_suffix(unit).map(|number| (number, unit)))
        .ok_or("Expected a positive duration with ms, s, m or h.")?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Expected a positive integer duration.");
    }
    let multiplier = match unit {
        "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        _ => 3_600_000,
    };
    let millis = number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .filter(|millis| (1..=86_400_000).contains(millis))
        .ok_or("Duration must be positive and at most 24 hours.")?;
    Ok(Duration::from_millis(millis))
}

/// Caller-owned existing SDK, identity and transport; no provider is installed.
pub struct ListHost<'a> {
    /// Immutable selected Fabric request context.
    pub client: &'a Client,
    /// Existing bounded item discovery for name-qualified owners.
    pub items: &'a dyn ItemExecutor,
    /// Existing bounded workspace discovery for name-qualified owners.
    pub workspaces: &'a dyn WorkspaceExecutor,
    /// Existing native operation executor.
    pub operations: &'a dyn OperationExecutor,
}

/// Fully rendered result, ready for one explicitly selected output sink.
pub struct ListResult {
    bytes: Vec<u8>,
    complete: bool,
}

impl ListResult {
    /// Exact newline-terminated JSON, with data-only projection already applied.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// A remaining token or URI is incomplete coverage, never successful exhaustion.
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.complete)
    }
}

impl ListHost<'_> {
    /// Resolve one exact owner and acquire one native datasource page.
    ///
    /// The original interruption future covers discovery, dispatch, decoding,
    /// pre-projection accounting and rendering. Tokens stay opaque and encode
    /// exactly once in the SDK; no URI, item reference or continuation is followed.
    ///
    /// # Errors
    /// Preserves typed read/resolution failures; never fabricates an empty page.
    pub async fn execute(
        &self,
        command: &ListCommand,
        observed_at: &str,
        interrupted: impl Future<Output = ListInterruption> + Send,
    ) -> Result<ListResult, ListError> {
        let mut interrupted = pin!(interrupted);
        let mut work = pin!(self.acquire(command, observed_at));
        poll_fn(|cx| {
            if let Poll::Ready(signal) = interrupted.as_mut().poll(cx) {
                return Poll::Ready(Err(ListError::Interrupted(signal)));
            }
            match work.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    if let Poll::Ready(signal) = interrupted.as_mut().poll(cx) {
                        Poll::Ready(Err(ListError::Interrupted(signal)))
                    } else {
                        Poll::Ready(result)
                    }
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    async fn acquire(
        &self,
        command: &ListCommand,
        observed_at: &str,
    ) -> Result<ListResult, ListError> {
        let response_bound = NonZeroUsize::new(RESPONSE_BYTES).expect("finite constant");
        let budget = GetReadBudget::new(
            self.items,
            self.workspaces,
            self.operations,
            None,
            32 * 1024 * 1024,
            21,
        );
        let resolved = resolve(
            command.owner.clone(),
            self.client,
            &budget,
            &budget,
            response_bound,
            pending(),
        )
        .await
        .map_err(|error| ListError::Resolution(Box::new(error)))?;
        let ResolvedGetTarget::Item {
            workspace_id,
            item_id,
            ..
        } = resolved
        else {
            unreachable!("parser restricts the collection owner");
        };
        let target = DataAgentTarget::new(&workspace_id, &item_id, "DataAgent")
            .map_err(|error| ListError::Read(Box::new(error)))?;
        let page = DataAgentRequests::datasources_page(
            &target,
            command.stage,
            command.continuation.as_ref(),
        )
        .map_err(|error| ListError::Read(Box::new(error)))?
        .execute(
            self.client,
            &budget,
            RepresentationLimits {
                max_response_bytes: response_bound,
                max_result_bytes: response_bound,
            },
            pending(),
        )
        .await
        .map_err(|error| ListError::Read(Box::new(error)))?;
        let complete = page.is_complete();
        let data = Value::Array(
            page.page
                .value
                .iter()
                .map(|row| row.as_value().clone())
                .collect(),
        );
        let data_bytes = serde_json::to_vec(&data)
            .map_err(|_| ListError::Output)?
            .len();
        if page.page.value.len() > command.limit.get() || data_bytes > command.max_bytes.get() {
            // A native token resumes after the whole page, not after a discarded row.
            return Err(ListError::BudgetExceeded);
        }
        let mut envelope = json!({
            "schemaVersion":"0.1",
            "status":if complete { "succeeded" } else { "partial" },
            "data":data,
            "meta":{
                "target":{"path":command.path,"workspaceId":workspace_id,"itemId":item_id,
                    "stage":if command.stage == Stage::Staging { "draft" } else { "published" },
                    "nativeStage":command.stage},
                "context":{"account":self.client.selection().account(),
                    "tenant":self.client.selection().tenant(),
                    "endpoint":self.client.context().endpoint().host()},
                "observation":{"observedAt":observed_at,"snapshot":false},
                "provenance":{"provider":"Fabric DataAgent configuration Preview","datasourcePages":1},
                "coverage":{"complete":complete,"scope":"selected-agent-stage","visibility":"service-authorized"},
                "truncation":{"truncated":false,"returnedRecords":page.page.value.len(),"dataBytes":data_bytes},
                "continuation":{"token":page.next_token.as_ref().map(ContinuationToken::as_str),
                    "uri":page.page.continuation_uri,"uriFollowed":false,"resumable":page.next_token.is_some()}
            }
        });
        if let Some(query) = &command.project {
            envelope["data"] = fabric_cli_query::project_value(&envelope["data"], query)
                .map_err(ListError::Projection)?;
        }
        let mut bytes = serde_json::to_vec(&envelope).map_err(|_| ListError::Output)?;
        bytes.push(b'\n');
        if bytes.len() > OUTPUT_BYTES {
            return Err(ListError::BudgetExceeded);
        }
        Ok(ListResult { bytes, complete })
    }
}

/// Exclusive-create local output preflight, performed before identity setup.
pub struct ListDelivery(Option<ExactOutputFile>);

impl ListDelivery {
    /// Inspect the exact local destination without creating or replacing it.
    ///
    /// # Errors
    /// Rejects existing destinations, invalid parents and nonabsolute working directories.
    pub fn preflight(command: &ListCommand, cwd: &Path) -> Result<Self, ListError> {
        if !cwd.is_absolute() {
            return Err(ListError::Output);
        }
        let file = command
            .output
            .as_ref()
            .map(|path| {
                ExactOutputFile::preflight(
                    &if path.is_absolute() {
                        path.clone()
                    } else {
                        cwd.join(path)
                    },
                    OUTPUT_BYTES,
                )
                .map_err(|error| ListError::File(Box::new(error)))
            })
            .transpose()?;
        Ok(Self(file))
    }

    /// Write one rendered envelope or fail explicitly; partial writes are not rolled back.
    ///
    /// # Errors
    /// Retains file errors and rejects stream write/flush failures.
    pub fn deliver(self, result: &ListResult, stdout: &mut dyn Write) -> Result<u8, ListError> {
        if let Some(file) = self.0 {
            file.commit(result.bytes())
                .map_err(|error| ListError::File(Box::new(error)))?;
        } else {
            stdout
                .write_all(result.bytes())
                .and_then(|()| stdout.flush())
                .map_err(|_| ListError::Output)?;
        }
        Ok(result.exit_code())
    }

    /// Supervise blocking output on the caller's existing native runtime.
    ///
    /// Interruption stops waiting, not an already-blocked operating-system
    /// write. The caller must shut down its runtime without waiting for blocked
    /// output, and report uncertain delivery rather than rollback or success.
    ///
    /// # Errors
    /// Retains delivery failures and distinguishes interruption after dispatch.
    #[cfg(feature = "native-host")]
    pub async fn deliver_supervised(
        self,
        result: ListResult,
        mut stdout: impl Write + Send + 'static,
        interrupted: impl Future<Output = ListInterruption> + Send,
    ) -> Result<u8, ListError> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let mut interrupted = pin!(interrupted);
        if let Poll::Ready(signal) = poll_fn(|cx| Poll::Ready(interrupted.as_mut().poll(cx))).await
        {
            return Err(ListError::Interrupted(signal));
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = stopped.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            if worker_stopped.load(Ordering::SeqCst) {
                return Err(ListError::Output);
            }
            self.deliver(&result, &mut stdout)
        });
        tokio::select! {
            biased;
            signal = interrupted.as_mut() => {
                stopped.store(true, Ordering::SeqCst);
                worker.abort();
                Err(ListError::DeliveryInterrupted(signal))
            }
            outcome = &mut worker => {
                if let Poll::Ready(signal) = poll_fn(|cx| Poll::Ready(interrupted.as_mut().poll(cx))).await {
                    return Err(ListError::DeliveryInterrupted(signal));
                }
                outcome.map_err(|_| ListError::Output)?
            }
        }
    }
}
