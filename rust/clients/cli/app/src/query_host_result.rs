// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Strict native bridge, rich codec and standard file-delivery query composition.

use super::{NativeQueryResult, QueryHostError, QueryIdentity, QueryObservation};
use crate::tabular_export::{ExportDelivery, ExportRequest, ExportSink};
use fabric_cli_output::{OutputStream, RenderPlan, tabular::TabularFormat, text::TextPlan};
use fabric_cli_query_commands::{QueryEncodingError, encode_presentation, encode_single};
use fabric_sdk::runtime::NativeDeadline;
use fabric_sdk_aihub_query::QueryOutcome;
use fabric_sdk_query_tabular::{
    AihubTabularResult, ConversionControl, ConversionError, ConversionLimits, KqlTabularResult,
    convert_aihub, convert_kql,
};
use fabric_sdk_tabular::{OutputLimits, ResultFormat, TabularResult};
use serde_json::{Value, json};
use std::{
    fmt,
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
};

/// Every converted native dataset and its unchanged identities and metadata.
pub enum ConvertedQuery {
    /// Strictly typed DataAgent resources, auxiliary resources and protocol data.
    AiHub(AihubTabularResult),
    /// Native KQL tables, diagnostics, metadata, terminal frame and evidence.
    Kql(KqlTabularResult),
}

impl ConvertedQuery {
    fn tables(&self) -> Vec<&TabularResult> {
        match self {
            Self::AiHub(result) => result.datasets.iter().map(|set| &set.table).collect(),
            Self::Kql(result) => result.datasets.iter().map(|set| &set.table).collect(),
        }
    }
}

/// Native/codec/file failure retained independently of the standard receipt.
#[derive(Debug, thiserror::Error)]
pub enum QueryDeliveryError {
    /// Exact native failure or incomplete result; no fallback is permitted.
    #[error(transparent)]
    Native(#[from] QueryHostError),
    /// Strict declared-type conversion failed after the query observation.
    #[error(transparent)]
    Conversion(#[from] ConversionError),
    /// All native sets are retained; encoding does not select or infer one.
    #[error(transparent)]
    Encoding(#[from] QueryEncodingError),
    /// Local cancellation wins before final file publication.
    #[error("Query output observation was canceled locally; do not replay the query")]
    Canceled,
    /// The original shared native budget elapsed before final publication.
    #[error("The query output deadline elapsed; do not replay the query")]
    Deadline,
    /// File publication failed; completed query and encoded-data evidence survive.
    #[error("Atomic query file delivery failed; retain the query evidence and do not replay")]
    File(#[source] io::Error),
}

/// Completed conversion or truthful failure, retaining the original query evidence.
pub struct QueryDelivery {
    /// Exact requested artifact, execution artifact and provider binding.
    pub identity: QueryIdentity,
    /// Original explicit format and destination, if supplied.
    pub export: Option<ExportRequest>,
    /// Every successfully converted set, even when encoding or file delivery fails.
    pub converted: Option<ConvertedQuery>,
    /// Standard execution/file receipt; no data bytes are inserted into it.
    pub receipt: ExportDelivery,
    /// Normal no-export rich JSON, including its trailing newline.
    pub presentation: Option<Vec<u8>>,
    /// Actual typed failure, never an invitation to dispatch again.
    pub error: Option<QueryDeliveryError>,
}

impl QueryObservation {
    /// Convert actual native models and encode once, retaining every native set.
    ///
    /// `conversion` borrows the caller's existing absolute deadline/cancel state;
    /// `deadline` is the same native budget used by SDK dispatch. The standard
    /// sink must be the one preflighted during query preparation. No service call,
    /// target re-resolution, inferred schema or fallback occurs in this phase.
    pub fn finish(
        self,
        conversion_limits: ConversionLimits,
        conversion: &ConversionControl<'_>,
        output_limits: OutputLimits,
        deadline: &NativeDeadline,
        canceled: &AtomicBool,
        sink: &mut impl ExportSink,
    ) -> QueryDelivery {
        let execution = execution_snapshot(&self.result);
        let mut delivery = QueryDelivery {
            identity: self.identity,
            export: self.export,
            converted: None,
            receipt: ExportDelivery {
                receipt: json!({
                    "source":"query",
                    "execution":execution,
                    "delivery":{"status":"not_started"}
                }),
                exit_code: 1,
            },
            presentation: None,
            error: None,
        };
        let result: Result<(), QueryDeliveryError> = (|| {
            let native = self.result?;
            check_output_control(deadline, canceled)?;
            let converted = match native {
                NativeQueryResult::AiHub(result) => ConvertedQuery::AiHub(convert_aihub(
                    QueryOutcome::Complete(result),
                    conversion_limits,
                    conversion,
                )?),
                NativeQueryResult::Kql(result) => {
                    ConvertedQuery::Kql(convert_kql(result, conversion_limits, conversion)?)
                }
            };
            delivery.converted = Some(converted);
            let tables = delivery
                .converted
                .as_ref()
                .expect("converted result was retained")
                .tables();
            check_output_control(deadline, canceled)?;
            let Some(request) = &delivery.export else {
                let bytes = encode_presentation(&tables, output_limits)?;
                check_output_control(deadline, canceled)?;
                delivery.presentation = Some(bytes);
                delivery.receipt.receipt["delivery"] =
                    json!({"status":"prepared","stage":"presentation"});
                return Ok(());
            };
            let format = match request.format {
                TabularFormat::Json => ResultFormat::Json,
                TabularFormat::Csv => ResultFormat::Csv,
                TabularFormat::Parquet => ResultFormat::Parquet,
            };
            let bytes = encode_single(&tables, format, output_limits)?;
            check_output_control(deadline, canceled)?;
            // encode_single has already established exactly one declared set.
            let table = tables[0];
            let schema = table
                .schema()
                .fields()
                .iter()
                .map(|field| {
                    json!({
                        "name":field.name(),
                        "type":field.data_type().to_string(),
                        "nullable":field.is_nullable()
                    })
                })
                .collect::<Vec<_>>();
            delivery.receipt.receipt["delivery"] = json!({
                "status":"not_started",
                "stage":"file",
                "file":request.output,
                "format":format_name(format),
                "rows":table.num_rows(),
                "bytes":bytes.len(),
                "schema":schema
            });
            sink.publish(request, &bytes)
                .map_err(QueryDeliveryError::File)?;
            delivery.receipt.receipt["delivery"]["status"] = json!("succeeded");
            Ok(())
        })();
        match result {
            Ok(()) => delivery.receipt.exit_code = 0,
            Err(error) => {
                let (stage, code) = error_code(&error);
                delivery.receipt.receipt["delivery"]["status"] = json!("failed");
                delivery.receipt.receipt["delivery"]["stage"] = json!(stage);
                delivery.receipt.receipt["delivery"]["error"] = json!({
                    "code":code,
                    "message":error.to_string()
                });
                delivery.error = Some(error);
            }
        }
        delivery
    }
}

/// Local output failure retaining every original query and file-delivery outcome.
#[derive(thiserror::Error)]
#[error("Writing query output failed; query and file evidence are retained without replay")]
pub struct QueryWriteFailure {
    /// Actual local writer error, not a remote query status.
    #[source]
    pub error: io::Error,
    /// Original query identity, converted results, receipt and intended data bytes.
    pub delivery: Box<QueryDelivery>,
}

impl fmt::Debug for QueryWriteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueryWriteFailure")
            .field("kind", &self.error.kind())
            .finish_non_exhaustive()
    }
}

impl QueryWriteFailure {
    /// Preserve typed execution evidence through the host's normal I/O boundary.
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.error.kind(), self)
    }
}

impl QueryDelivery {
    /// Use the existing standard byte plan; no table inference or receipt mixing.
    pub fn plan(&self) -> io::Result<TextPlan> {
        if let Some(bytes) = &self.presentation {
            Ok(TextPlan {
                records: vec![RenderPlan {
                    stream: OutputStream::Stdout,
                    exit_code: self.receipt.exit_code,
                    payload: bytes.clone(),
                }],
                exit_code: self.receipt.exit_code,
            })
        } else {
            self.receipt.plan()
        }
    }

    /// Write once through the standard plan while retaining evidence on I/O failure.
    ///
    /// Normal presentation contains only rich data; file output contains only the
    /// standard receipt. No method retries encoding, file publication, or queries.
    pub fn write_to_streams(
        self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<u8, QueryWriteFailure> {
        let written = self
            .plan()
            .and_then(|plan| plan.write_to(stdout, stderr))
            .and_then(|()| stdout.flush());
        match written {
            Ok(()) => Ok(self.receipt.exit_code),
            Err(error) => Err(QueryWriteFailure {
                error,
                delivery: Box::new(self),
            }),
        }
    }
}

fn check_output_control(
    deadline: &NativeDeadline,
    canceled: &AtomicBool,
) -> Result<(), QueryDeliveryError> {
    if canceled.load(Ordering::SeqCst) {
        return Err(QueryDeliveryError::Canceled);
    }
    deadline
        .remaining()
        .map_err(|_| QueryDeliveryError::Deadline)?;
    Ok(())
}

fn format_name(format: ResultFormat) -> &'static str {
    match format {
        ResultFormat::Json => "json",
        ResultFormat::Csv => "csv",
        ResultFormat::Parquet => "parquet",
    }
}

fn error_code(error: &QueryDeliveryError) -> (&'static str, &'static str) {
    match error {
        QueryDeliveryError::Native(_) => ("execution", "native_query_failed"),
        QueryDeliveryError::Conversion(_) => ("conversion", "query_type_conversion_failed"),
        QueryDeliveryError::Encoding(_) => ("encoding", "query_encoding_failed"),
        QueryDeliveryError::Canceled => ("delivery", "query_output_canceled"),
        QueryDeliveryError::Deadline => ("delivery", "query_output_deadline"),
        QueryDeliveryError::File(_) => ("file", "query_file_delivery_failed"),
    }
}

fn execution_snapshot(result: &Result<NativeQueryResult, QueryHostError>) -> Value {
    match result {
        Ok(NativeQueryResult::AiHub(result)) => json!({
            "status":"succeeded",
            "metadata":{
                "provider":"aihub",
                "protocolVersion":result.protocol_version,
                "resultsets":result.resultsets.iter().enumerate().map(|(ordinal,set)| json!({
                    "ordinal":ordinal,"uri":set.uri,"schemaVersion":set.schema_version,
                    "resultId":set.result_id,"rowCount":set.row_count,
                    "truncated":set.truncated,"maxRows":set.max_rows,
                    "totalRowCount":set.total_row_count,
                    "columns":set.columns.iter().map(|column| json!({
                        "name":column.name,"engineType":column.engine_type,
                        "logicalType":column.logical_type,"encoding":column.encoding
                    })).collect::<Vec<_>>()
                })).collect::<Vec<_>>(),
                "auxiliaryResources":result.resources.len()
            }
        }),
        Ok(NativeQueryResult::Kql(result)) => json!({
            "status":"succeeded",
            "metadata":{
                "provider":"kql","evidence":result.evidence,
                "resultsets":result.datasets.iter().map(|set| json!({
                    "ordinal":set.ordinal,"tableId":set.table_id,"name":set.name,
                    "columns":set.columns,"rows":set.rows.len()
                })).collect::<Vec<_>>()
            }
        }),
        Err(QueryHostError::AiHub(error)) => json!({
            "status":if error.query_dispatched { "unknown" } else { "not_started" },
            "metadata":{"provider":"aihub","queryDispatched":error.query_dispatched}
        }),
        Err(QueryHostError::IncompleteAiHub(result)) => json!({
            "status":"partial",
            "metadata":{"provider":"aihub","protocolVersion":result.protocol_version,
                "resultsets":result.resultsets.len()}
        }),
        Err(QueryHostError::Kql(error)) => json!({
            "status":if error.partial.is_some() { "partial" }
                else if error.evidence.is_some() { "unknown" } else { "not_started" },
            "metadata":{"provider":"kql","evidence":error.evidence}
        }),
        Err(_) => json!({"status":"not_started"}),
    }
}
