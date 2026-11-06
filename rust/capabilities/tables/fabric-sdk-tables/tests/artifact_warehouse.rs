// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native-executor boundary tests, not live TDS/permission proof.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk_tables::warehouse::*;
use std::{
    collections::VecDeque,
    future::{pending, poll_fn},
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const LAKEHOUSE: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SERVER: &str = "example.datawarehouse.fabric.microsoft.com";
const TOKEN: &str = "offline-sql-fixture-only";

#[derive(Debug)]
struct Control {
    origin: Instant,
    seconds: AtomicU64,
    cancelled: AtomicBool,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            seconds: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        }
    }
}

impl SqlControl for Control {
    fn now(&self) -> Instant {
        self.origin + Duration::from_secs(self.seconds.load(Ordering::SeqCst))
    }

    fn cancelled(&self) -> SqlFuture<'_, ()> {
        Box::pin(poll_fn(|_| {
            if self.cancelled.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }

    fn sleep_until(&self, deadline: Instant) -> SqlFuture<'_, ()> {
        Box::pin(poll_fn(move |_| {
            if self.now() >= deadline {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum CredentialAction {
    #[default]
    Token,
    Failure,
    Pending,
}

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    action: CredentialAction,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).to_owned()).collect());
        match self.action {
            CredentialAction::Token => Ok(AccessToken {
                token: TOKEN.into(),
                expires_on: OffsetDateTime::UNIX_EPOCH,
            }),
            CredentialAction::Failure => Err(azure_core::Error::message(
                azure_core::error::ErrorKind::Other,
                "offline credential failure",
            )),
            CredentialAction::Pending => pending().await,
        }
    }
}

#[derive(Debug)]
struct InterruptingCredential {
    control: Arc<Control>,
    deadline: bool,
    return_token: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl TokenCredential for InterruptingCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, &[SQL_SCOPE]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.deadline {
            self.control.seconds.store(121, Ordering::SeqCst);
        } else {
            self.control.cancelled.store(true, Ordering::SeqCst);
        }
        if !self.return_token {
            poll_fn(|cx| {
                cx.waker().wake_by_ref();
                Poll::<()>::Pending
            })
            .await;
        }
        Ok(AccessToken {
            token: TOKEN.into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

#[derive(Clone)]
struct Recorded {
    statement: String,
    operation: WarehouseOperation,
    read_only: bool,
    limits: SqlLimits,
    deadline: Instant,
}

enum Step {
    Outcome(SqlOutcome),
    Failure(SqlExecutionError),
    Cancel(Arc<Control>),
    Deadline(Arc<Control>),
}

struct Executor {
    steps: Mutex<VecDeque<Step>>,
    recorded: Mutex<Vec<Recorded>>,
    read_only: SqlReadOnlySupport,
    drops: Arc<AtomicUsize>,
}

impl Executor {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            recorded: Mutex::new(Vec::new()),
            read_only: SqlReadOnlySupport::ServerEnforced,
            drops: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn successful() -> Self {
        Self::new([Step::Outcome(completed())])
    }

    fn records(&self) -> Vec<Recorded> {
        self.recorded.lock().unwrap().clone()
    }
}

struct DropGuard(Arc<AtomicUsize>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl SqlExecutor for Executor {
    fn read_only_support(&self) -> SqlReadOnlySupport {
        self.read_only
    }

    fn execute<'a>(
        &'a self,
        request: SqlRequest<'a>,
        control: &'a dyn SqlControl,
    ) -> SqlFuture<'a, Result<SqlOutcome, SqlExecutionError>> {
        Box::pin(async move {
            let _guard = DropGuard(self.drops.clone());
            assert_eq!(request.connection().server(), SERVER);
            assert_eq!(request.connection().port(), 1433);
            assert_eq!(request.connection().initial_catalog(), "exact catalog");
            assert_eq!(request.token().token.secret(), TOKEN);
            let debug = format!("{request:?}");
            assert!(!debug.contains(TOKEN));
            assert!(!debug.contains(request.statement()));
            assert!(request.deadline() > control.now());
            self.recorded.lock().unwrap().push(Recorded {
                statement: request.statement().to_owned(),
                operation: request.operation().clone(),
                read_only: request.read_only_required(),
                limits: request.limits(),
                deadline: request.deadline(),
            });
            let step = self.steps.lock().unwrap().pop_front().expect("no replay");
            match step {
                Step::Outcome(outcome) => Ok(outcome),
                Step::Failure(error) => Err(error),
                Step::Cancel(control) => {
                    control.cancelled.store(true, Ordering::SeqCst);
                    poll_fn(|cx| {
                        cx.waker().wake_by_ref();
                        Poll::<()>::Pending
                    })
                    .await;
                    unreachable!()
                }
                Step::Deadline(control) => {
                    control.seconds.store(121, Ordering::SeqCst);
                    poll_fn(|cx| {
                        cx.waker().wake_by_ref();
                        Poll::<()>::Pending
                    })
                    .await;
                    unreachable!()
                }
            }
        })
    }
}

fn client(credential: Arc<Credential>) -> WarehouseClient {
    WarehouseClient::new(
        credential,
        WarehouseConnection::new(SERVER, "exact catalog").unwrap(),
    )
}

fn table() -> WarehouseTable {
    WarehouseTable::new("sales", "Orders").unwrap()
}

fn source() -> LakehouseFilesSource {
    LakehouseFilesSource::new(
        WORKSPACE,
        LAKEHOUSE,
        OneLakeSourceItemKind::Lakehouse,
        "Files/ingest/orders.csv",
    )
    .unwrap()
}

fn options() -> CopyIntoOptions {
    CopyIntoOptions::new(CopyFileType::Csv, CopySourceAuthorization::CallerIdentity)
}

fn completed() -> SqlOutcome {
    SqlOutcome {
        events: vec![SqlEvent::Done {
            rows_affected: None,
        }],
        terminal: SqlTerminal::Completed,
        decoded_bytes: 8,
    }
}

fn set(names: &[&str], rows: Vec<Vec<SqlValue>>) -> SqlEvent {
    SqlEvent::ResultSet(SqlResultSet {
        columns: names
            .iter()
            .map(|name| SqlColumn {
                name: (*name).to_owned(),
                native_type: "native SQL type".to_owned(),
                nullable: None,
            })
            .collect(),
        rows,
        complete: true,
    })
}

fn diagnostic(message: &str) -> SqlDiagnostic {
    SqlDiagnostic {
        number: 1234,
        state: 2,
        class: 16,
        message: message.to_owned(),
        procedure: Some("native_proc".into()),
        line: Some(11),
    }
}

fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

#[test]
fn connection_binding_rejects_urls_connection_strings_untrusted_hosts_and_catalogs() {
    for server in [
        "",
        "datawarehouse.fabric.microsoft.com",
        ".datawarehouse.fabric.microsoft.com",
        "evil-datawarehouse.fabric.microsoft.com",
        "example.datawarehouse.fabric.microsoft.com.evil.test",
        "example.datawarehouse.fabric.microsoft.com:1433",
        "tcp:example.datawarehouse.fabric.microsoft.com,1433",
        "https://example.datawarehouse.fabric.microsoft.com",
        "example.datawarehouse.fabric.microsoft.com;TrustServerCertificate=true",
        "user@example.datawarehouse.fabric.microsoft.com",
        "example..datawarehouse.fabric.microsoft.com",
        "-example.datawarehouse.fabric.microsoft.com",
        "example_.datawarehouse.fabric.microsoft.com",
        " example.datawarehouse.fabric.microsoft.com",
    ] {
        assert_eq!(
            WarehouseConnection::new(server, "catalog")
                .unwrap_err()
                .kind,
            WarehouseErrorKind::InvalidInput,
            "{server}"
        );
    }
    for catalog in ["", " ", "catalog;Encrypt=false", "catalog\0", "catalog\n"] {
        assert!(WarehouseConnection::new(SERVER, catalog).is_err());
    }
    assert!(WarehouseConnection::new(SERVER, &"x".repeat(129)).is_err());
    let binding = WarehouseConnection::new(
        "EXAMPLE.DATAWAREHOUSE.FABRIC.MICROSOFT.COM",
        "exact ' [catalog].name",
    )
    .unwrap();
    assert_eq!(binding.server(), SERVER);
    assert_eq!(binding.initial_catalog(), "exact ' [catalog].name");
    assert!(!format!("{binding:?}").contains("exact ' [catalog].name"));
}

#[test]
fn identifiers_are_exact_not_split_or_unquoted_and_are_utf16_bounded() {
    assert_eq!(
        WarehouseTable::new("a.b]'", " c.d]' ").unwrap().quoted(),
        "[a.b]]'].[ c.d]]' ]"
    );
    assert_eq!(
        SqlIdentifier::new("[already]").unwrap().quoted(),
        "[[already]]]"
    );
    for value in ["", " ", "\0", "x\n", "x\t"] {
        assert!(SqlIdentifier::new(value).is_err());
    }
    assert!(SqlIdentifier::new("x".repeat(128)).is_ok());
    assert!(SqlIdentifier::new("x".repeat(129)).is_err());
    assert!(SqlIdentifier::new("😀".repeat(64)).is_ok());
    assert!(SqlIdentifier::new("😀".repeat(65)).is_err());
}

#[test]
fn sources_require_guid_resolved_lakehouse_files_without_secrets_or_url_ambiguity() {
    for kind in [
        OneLakeSourceItemKind::Warehouse,
        OneLakeSourceItemKind::Other,
    ] {
        assert!(LakehouseFilesSource::new(WORKSPACE, LAKEHOUSE, kind, "Files/source.csv").is_err());
    }
    for workspace in [
        "workspace-name",
        "",
        "{11111111-2222-3333-4444-555555555555}",
    ] {
        assert!(
            LakehouseFilesSource::new(
                workspace,
                LAKEHOUSE,
                OneLakeSourceItemKind::Lakehouse,
                "Files/source.csv"
            )
            .is_err()
        );
    }
    assert!(
        LakehouseFilesSource::new(
            WORKSPACE,
            "warehouse-name",
            OneLakeSourceItemKind::Lakehouse,
            "Files/source.csv"
        )
        .is_err()
    );
    for path in [
        "",
        "Files",
        "files/data.csv",
        "/Files/data.csv",
        "Tables/data.csv",
        "Files/",
        "Files//data.csv",
        "Files/./data.csv",
        "Files/../data.csv",
        "Files/%2e%2e/data.csv",
        "Files/data.csv?sig=secret",
        "Files/data.csv#fragment",
        "Files/data.csv;secret=key",
        "Files\\data.csv",
        "Files/*.csv",
        "Files/data\0.csv",
        "https://storage.blob.core.windows.net/container/file",
    ] {
        assert!(
            LakehouseFilesSource::new(WORKSPACE, LAKEHOUSE, OneLakeSourceItemKind::Lakehouse, path)
                .is_err(),
            "{path}"
        );
    }
    let source = LakehouseFilesSource::new(
        WORKSPACE,
        LAKEHOUSE,
        OneLakeSourceItemKind::Lakehouse,
        "Files/naïve space/O'Brien.csv",
    )
    .unwrap();
    assert_eq!(
        source.url().as_str(),
        format!(
            "https://onelake.dfs.fabric.microsoft.com/{WORKSPACE}/{LAKEHOUSE}/Files/na%C3%AFve%20space/O'Brien.csv"
        )
    );
    assert!(source.url().query().is_none());
    assert!(source.url().fragment().is_none());
    assert!(!format!("{source:?}").contains("O'Brien"));
}

#[test]
fn advertised_formats_and_source_auth_have_no_jsonl_sas_or_key_fallback() {
    assert_eq!("csv".parse::<CopyFileType>().unwrap(), CopyFileType::Csv);
    assert_eq!(
        "PARQUET".parse::<CopyFileType>().unwrap(),
        CopyFileType::Parquet
    );
    for format in ["jsonl", "JSONL", "orc", "", "csv; DROP TABLE x", "parquet "] {
        assert!(format.parse::<CopyFileType>().is_err());
    }
    assert_eq!(
        "caller-identity"
            .parse::<CopySourceAuthorization>()
            .unwrap(),
        CopySourceAuthorization::CallerIdentity
    );
    assert_eq!(
        "workspace-identity"
            .parse::<CopySourceAuthorization>()
            .unwrap(),
        CopySourceAuthorization::WorkspaceIdentity
    );
    for auth in [
        "",
        "sas",
        "shared-key",
        "unknown",
        "caller-identity;SECRET=x",
    ] {
        assert!(auth.parse::<CopySourceAuthorization>().is_err());
    }
}

#[tokio::test]
async fn metadata_and_schema_use_exact_schema_table_filters_and_ordered_native_columns() {
    let executor = Executor::new([Step::Outcome(completed()), Step::Outcome(completed())]);
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let control = Control::default();
    let table = WarehouseTable::new("sales.'x", "Orders.]'").unwrap();
    client
        .metadata(&executor, &table, SqlLimits::default(), &control)
        .await
        .unwrap();
    client
        .schema(&executor, &table, SqlLimits::default(), &control)
        .await
        .unwrap();
    let records = executor.records();
    assert_eq!(
        records[0].statement,
        "SELECT TABLE_CATALOG, TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE \
         FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_SCHEMA = N'sales.''x' \
         AND TABLE_NAME = N'Orders.]''';"
    );
    assert_eq!(
        records[1].statement,
        "SELECT TABLE_CATALOG, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, \
         ORDINAL_POSITION, DATA_TYPE, IS_NULLABLE, CHARACTER_MAXIMUM_LENGTH, \
         NUMERIC_PRECISION, NUMERIC_SCALE, DATETIME_PRECISION, COLUMN_DEFAULT \
         FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA = N'sales.''x' \
         AND TABLE_NAME = N'Orders.]''' ORDER BY ORDINAL_POSITION;"
    );
    assert_eq!(
        records[0].operation,
        WarehouseOperation::Metadata(table.clone())
    );
    assert_eq!(records[1].operation, WarehouseOperation::Schema(table));
    assert!(records.iter().all(|record| record.read_only));
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![SQL_SCOPE.to_owned()], vec![SQL_SCOPE.to_owned()]]
    );
}

#[tokio::test]
async fn statistics_selection_is_exact_table_scoped_not_statistic_name_only() {
    let executor = Executor::successful();
    client(Arc::new(Credential::default()))
        .statistics(
            &executor,
            &WarehouseTable::new("a'b", "t]x").unwrap(),
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    let records = executor.records();
    assert_eq!(
        records[0].statement,
        "SELECT s.stats_id, s.name AS statistic_name, \
         STATS_DATE(s.object_id, s.stats_id) AS last_updated, \
         sc.stats_column_id, c.name AS column_name \
         FROM sys.stats AS s \
         JOIN sys.tables AS t ON t.object_id = s.object_id \
         JOIN sys.schemas AS sch ON sch.schema_id = t.schema_id \
         JOIN sys.stats_columns AS sc ON sc.object_id = s.object_id \
         AND sc.stats_id = s.stats_id \
         JOIN sys.columns AS c ON c.object_id = sc.object_id \
         AND c.column_id = sc.column_id \
         WHERE sch.name = N'a''b' AND t.name = N't]x' \
         ORDER BY s.stats_id, sc.stats_column_id;"
    );
}

#[tokio::test]
async fn dbcc_preserves_all_ordered_header_density_histogram_sets_and_native_counts() {
    let expected = SqlOutcome {
        events: vec![
            set(
                &["Name", "Rows"],
                vec![vec![
                    SqlValue::Text("s'tat".into()),
                    SqlValue::Integer(9_007_199_254_740_993),
                ]],
            ),
            SqlEvent::Done {
                rows_affected: Some(1),
            },
            set(
                &["All density", "Columns"],
                vec![vec![
                    SqlValue::Float64Bits(0.1_f64.to_bits()),
                    SqlValue::Text("col".into()),
                ]],
            ),
            SqlEvent::Done {
                rows_affected: Some(1),
            },
            set(
                &["RANGE_HI_KEY", "EQ_ROWS"],
                vec![vec![
                    SqlValue::Decimal("9999999999999999999999999999.1234567890".into()),
                    SqlValue::Float64Bits(42.0_f64.to_bits()),
                ]],
            ),
            SqlEvent::Done {
                rows_affected: Some(1),
            },
        ],
        terminal: SqlTerminal::Completed,
        decoded_bytes: 512,
    };
    let executor = Executor::new([Step::Outcome(expected.clone())]);
    let outcome = client(Arc::new(Credential::default()))
        .show_statistics(
            &executor,
            &WarehouseTable::new("sales]'", "Orders.x").unwrap(),
            &SqlIdentifier::new("s'tat]").unwrap(),
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, expected);
    assert_eq!(
        executor.records()[0].statement,
        "DBCC SHOW_STATISTICS (N'[sales]]''].[Orders.x]', N's''tat]');"
    );
}

#[tokio::test]
async fn query_is_unchanged_read_contract_context_not_single_table_substitution() {
    let sql = "/* read batch, not a SELECT prefix */\n\
               WITH x AS (SELECT * FROM [sales].[Orders]) \
               SELECT x.id, other.id FROM x JOIN [other].[Table] other ON x.id = other.id;";
    let executor = Executor::successful();
    let context = table();
    client(Arc::new(Credential::default()))
        .query(
            &executor,
            &context,
            sql,
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    let records = executor.records();
    assert_eq!(records[0].statement, sql);
    assert_eq!(records[0].operation, WarehouseOperation::Query(context));
    assert!(records[0].read_only);
}

#[tokio::test]
async fn query_refuses_unenforced_executor_before_sql_token_or_dispatch() {
    let mut executor = Executor::new([]);
    executor.read_only = SqlReadOnlySupport::Unsupported;
    let credential = Arc::new(Credential::default());
    let error = client(credential.clone())
        .query(
            &executor,
            &table(),
            "SELECT 1; DELETE FROM [sales].[Orders];",
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::ReadOnlyUnavailable);
    assert_eq!(error.dispatch, SqlDispatch::NotDispatched);
    assert!(!error.mutation_may_have_occurred);
    assert!(executor.records().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn server_read_only_policy_rejects_mixed_batch_without_a_prefix_heuristic() {
    let expected = SqlOutcome {
        events: vec![
            set(&["n"], vec![vec![SqlValue::Integer(1)]]),
            SqlEvent::Error(diagnostic(
                "server denied DELETE under read-only permissions",
            )),
        ],
        terminal: SqlTerminal::Failed,
        decoded_bytes: 128,
    };
    let executor = Executor::new([Step::Outcome(expected.clone())]);
    let error = client(Arc::new(Credential::default()))
        .query(
            &executor,
            &table(),
            "SELECT 1; DELETE FROM [sales].[Orders];",
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Server);
    assert_eq!(error.dispatch, SqlDispatch::Terminal);
    assert_eq!(error.outcome.as_deref(), Some(&expected));
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(executor.records().len(), 1);
}

#[tokio::test]
async fn query_preserves_duplicate_columns_exact_scalars_null_empty_and_all_sets() {
    let values = vec![
        SqlValue::Integer(i64::MAX),
        SqlValue::Decimal("9999999999999999999999999999.1234567890".into()),
        SqlValue::Float32Bits(0x7fc0_0001),
        SqlValue::Float64Bits((-0.0_f64).to_bits()),
        SqlValue::Null,
        SqlValue::Text(String::new()),
        SqlValue::Binary(vec![]),
        SqlValue::Binary(vec![0, 255, 1]),
        SqlValue::Bit(false),
        SqlValue::Native {
            encoding: "tds:datetime:days-i32-ticks-u32-le".into(),
            bytes: vec![1, 0, 0, 0, 123, 0, 0, 0],
        },
        SqlValue::Text("{ \"number\": 9007199254740993 }".into()),
        SqlValue::Text("2026-11-05T17:00:00.1234567+01:00".into()),
    ];
    let names = vec!["duplicate"; values.len()];
    let expected = SqlOutcome {
        events: vec![
            SqlEvent::Information(diagnostic("native informational message")),
            set(&names, vec![values]),
            SqlEvent::Done {
                rows_affected: Some(1),
            },
            set(&["duplicate", "duplicate"], vec![]),
            SqlEvent::Done {
                rows_affected: Some(0),
            },
        ],
        terminal: SqlTerminal::Completed,
        decoded_bytes: 1024,
    };
    let executor = Executor::new([Step::Outcome(expected.clone())]);
    let actual = client(Arc::new(Credential::default()))
        .query(
            &executor,
            &table(),
            "SELECT native_values; SELECT empty;",
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn copy_csv_has_exact_escaped_identifiers_literals_options_and_no_other_effects() {
    let executor = Executor::successful();
    let credential = Arc::new(Credential::default());
    let mut options = options();
    options.columns = vec![
        SqlIdentifier::new("first]").unwrap(),
        SqlIdentifier::new("second.'").unwrap(),
    ];
    options.csv = Some(CsvOptions {
        first_row: NonZeroU32::new(2),
        field_terminator: Some("'".into()),
        row_terminator: Some("0x0A".into()),
        encoding: Some(CsvEncoding::Utf8),
    });
    let source = LakehouseFilesSource::new(
        WORKSPACE,
        LAKEHOUSE,
        OneLakeSourceItemKind::Lakehouse,
        "Files/O'Brien.csv",
    )
    .unwrap();
    let target = WarehouseTable::new("sales]'", "Orders.x").unwrap();
    client(credential.clone())
        .copy_into(
            &executor,
            &target,
            &source,
            &options,
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    let records = executor.records();
    assert_eq!(
        records[0].statement,
        format!(
            "COPY INTO [sales]]'].[Orders.x] ([first]]], [second.'])\n\
             FROM 'https://onelake.dfs.fabric.microsoft.com/{WORKSPACE}/{LAKEHOUSE}/Files/O''Brien.csv'\n\
             WITH (FILE_TYPE = 'CSV', MAXERRORS = 0, FIRSTROW = 2, FIELDTERMINATOR = '''', \
             ROWTERMINATOR = '0x0A', ENCODING = 'UTF8');"
        )
    );
    assert!(!records[0].read_only);
    assert_eq!(
        records[0].operation,
        WarehouseOperation::CopyInto {
            table: target,
            source,
            options,
        }
    );
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![SQL_SCOPE.to_owned()]]
    );
    for forbidden in [
        "TRUNCATE",
        "CREATE",
        "OVERWRITE",
        "ERRORFILE",
        "SECRET",
        "CREDENTIAL",
    ] {
        assert!(!records[0].statement.contains(forbidden));
    }
}

#[tokio::test]
async fn parquet_workspace_source_identity_does_not_change_sql_caller_or_token_scope() {
    let executor = Executor::successful();
    let credential = Arc::new(Credential::default());
    let options = CopyIntoOptions::new(
        CopyFileType::Parquet,
        CopySourceAuthorization::WorkspaceIdentity,
    );
    client(credential.clone())
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options,
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    let records = executor.records();
    assert_eq!(
        records[0].statement,
        format!(
            "COPY INTO [sales].[Orders]\n\
             FROM 'https://onelake.dfs.fabric.microsoft.com/{WORKSPACE}/{LAKEHOUSE}/Files/ingest/orders.csv'\n\
             WITH (FILE_TYPE = 'PARQUET', \
             CREDENTIAL = (IDENTITY = 'Workspace Identity'));"
        )
    );
    for forbidden in [
        "MAXERRORS",
        "FIRSTROW",
        "FIELDTERMINATOR",
        "ROWTERMINATOR",
        "ENCODING",
        "ERRORFILE",
        "Managed Identity",
    ] {
        assert!(!records[0].statement.contains(forbidden));
    }
    assert!(!records[0].read_only);
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![SQL_SCOPE.to_owned()]]
    );
}

#[tokio::test]
async fn parquet_caller_identity_omits_maxerrors_and_source_credential_clause() {
    let executor = Executor::successful();
    let credential = Arc::new(Credential::default());
    let options = CopyIntoOptions::new(
        CopyFileType::Parquet,
        CopySourceAuthorization::CallerIdentity,
    );
    client(credential.clone())
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options,
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    let records = executor.records();
    assert_eq!(
        records[0].statement,
        format!(
            "COPY INTO [sales].[Orders]\n\
             FROM 'https://onelake.dfs.fabric.microsoft.com/{WORKSPACE}/{LAKEHOUSE}/Files/ingest/orders.csv'\n\
             WITH (FILE_TYPE = 'PARQUET');"
        )
    );
    assert!(!records[0].statement.contains("MAXERRORS"));
    assert!(!records[0].statement.contains("CREDENTIAL"));
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![SQL_SCOPE.to_owned()]]
    );
}

#[tokio::test]
async fn csv_workspace_identity_retains_zero_error_limit_and_original_sql_login_scope() {
    let executor = Executor::successful();
    let credential = Arc::new(Credential::default());
    let options = CopyIntoOptions::new(
        CopyFileType::Csv,
        CopySourceAuthorization::WorkspaceIdentity,
    );
    client(credential.clone())
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options,
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    let records = executor.records();
    assert_eq!(
        records[0].statement,
        format!(
            "COPY INTO [sales].[Orders]\n\
             FROM 'https://onelake.dfs.fabric.microsoft.com/{WORKSPACE}/{LAKEHOUSE}/Files/ingest/orders.csv'\n\
             WITH (FILE_TYPE = 'CSV', MAXERRORS = 0, \
             CREDENTIAL = (IDENTITY = 'Workspace Identity'));"
        )
    );
    assert!(!records[0].statement.contains("Managed Identity"));
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![SQL_SCOPE.to_owned()]]
    );
}

#[tokio::test]
async fn invalid_structured_copy_options_fail_before_credentials_or_execution() {
    let mut invalid_options = Vec::new();
    let mut parquet = CopyIntoOptions::new(
        CopyFileType::Parquet,
        CopySourceAuthorization::CallerIdentity,
    );
    parquet.csv = Some(CsvOptions::default());
    invalid_options.push(parquet);
    for value in ["", "more-than-sixteen-bytes", "a\0b"] {
        let mut field_options = options();
        field_options.csv = Some(CsvOptions {
            field_terminator: Some(value.to_owned()),
            ..CsvOptions::default()
        });
        invalid_options.push(field_options);
        let mut row_options = options();
        row_options.csv = Some(CsvOptions {
            row_terminator: Some(value.to_owned()),
            ..CsvOptions::default()
        });
        invalid_options.push(row_options);
    }
    let mut duplicate = options();
    duplicate.columns = vec![
        SqlIdentifier::new("same").unwrap(),
        SqlIdentifier::new("same").unwrap(),
    ];
    invalid_options.push(duplicate);
    for options in invalid_options {
        let credential = Arc::new(Credential::default());
        let executor = Executor::new([]);
        let error = client(credential.clone())
            .copy_into(
                &executor,
                &table(),
                &source(),
                &options,
                SqlLimits::default(),
                &Control::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, WarehouseErrorKind::InvalidInput);
        assert_eq!(error.dispatch, SqlDispatch::NotDispatched);
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(executor.records().is_empty());
    }
}

#[tokio::test]
async fn scalar_terminator_injection_remains_a_literal_and_utf16_is_structured() {
    let executor = Executor::successful();
    let mut options = options();
    options.csv = Some(CsvOptions {
        field_terminator: Some("';--".into()),
        row_terminator: Some("\n".into()),
        encoding: Some(CsvEncoding::Utf16),
        ..CsvOptions::default()
    });
    client(Arc::new(Credential::default()))
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options,
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    let records = executor.records();
    assert!(records[0].statement.contains("FIELDTERMINATOR = ''';--'"));
    assert!(records[0].statement.contains("ROWTERMINATOR = '\n'"));
    assert!(records[0].statement.contains("ENCODING = 'UTF16'"));
}

#[tokio::test]
async fn empty_copy_summary_is_only_native_completion_not_a_fabric_job_or_row_claim() {
    let executor = Executor::new([Step::Outcome(SqlOutcome {
        events: vec![],
        terminal: SqlTerminal::Completed,
        decoded_bytes: 0,
    })]);
    let result = client(Arc::new(Credential::default()))
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap();
    assert!(result.events.is_empty());
    assert_eq!(result.terminal, SqlTerminal::Completed);
    assert_eq!(executor.records().len(), 1);
}

#[tokio::test]
async fn copy_retains_native_summary_and_server_errors_without_retry_or_rollback_claim() {
    let expected = SqlOutcome {
        events: vec![
            set(
                &["RowsLoaded", "RowsRejected"],
                vec![vec![SqlValue::Integer(17), SqlValue::Integer(1)]],
            ),
            SqlEvent::Error(diagnostic("source row failed native validation")),
        ],
        terminal: SqlTerminal::Failed,
        decoded_bytes: 256,
    };
    let executor = Executor::new([Step::Outcome(expected.clone())]);
    let error = client(Arc::new(Credential::default()))
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Server);
    assert_eq!(error.dispatch, SqlDispatch::Terminal);
    assert!(error.mutation_may_have_occurred);
    assert_eq!(error.outcome.as_deref(), Some(&expected));
    assert!(!format!("{error} {error:?}").contains("source row failed"));
    assert_eq!(executor.records().len(), 1);
}

#[tokio::test]
async fn terminal_completed_cannot_hide_error_frames_or_partial_result_sets() {
    for (events, kind, dispatch) in [
        (
            vec![SqlEvent::Error(diagnostic("native failure"))],
            WarehouseErrorKind::Server,
            SqlDispatch::Terminal,
        ),
        (
            vec![SqlEvent::ResultSet(SqlResultSet {
                columns: vec![],
                rows: vec![],
                complete: false,
            })],
            WarehouseErrorKind::Protocol,
            SqlDispatch::Unknown,
        ),
        (
            vec![set(&["column"], vec![vec![]])],
            WarehouseErrorKind::Protocol,
            SqlDispatch::Terminal,
        ),
    ] {
        let executor = Executor::new([Step::Outcome(SqlOutcome {
            events,
            terminal: SqlTerminal::Completed,
            decoded_bytes: 64,
        })]);
        let error = client(Arc::new(Credential::default()))
            .copy_into(
                &executor,
                &table(),
                &source(),
                &options(),
                SqlLimits::default(),
                &Control::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.dispatch, dispatch);
        assert!(error.mutation_may_have_occurred);
        assert!(error.outcome.is_some());
        assert_eq!(executor.records().len(), 1);
    }
}

#[tokio::test]
async fn missing_terminal_completion_is_unknown_even_with_completed_rows() {
    let expected = SqlOutcome {
        events: vec![set(&["rows"], vec![vec![SqlValue::Integer(10)]])],
        terminal: SqlTerminal::Incomplete,
        decoded_bytes: 64,
    };
    let executor = Executor::new([Step::Outcome(expected.clone())]);
    let error = client(Arc::new(Credential::default()))
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Protocol);
    assert_eq!(error.dispatch, SqlDispatch::Unknown);
    assert!(error.mutation_may_have_occurred);
    assert_eq!(error.outcome.as_deref(), Some(&expected));
    assert_eq!(executor.records().len(), 1);
}

#[tokio::test]
async fn transport_dispatch_evidence_and_partial_rows_are_preserved_never_replayed() {
    let partial = SqlOutcome {
        events: vec![set(&["rows"], vec![vec![SqlValue::Integer(10)]])],
        terminal: SqlTerminal::Incomplete,
        decoded_bytes: 64,
    };
    for (dispatch, evidence, expected_dispatch, expected_kind) in [
        (
            SqlDispatch::Unknown,
            None,
            SqlDispatch::Unknown,
            WarehouseErrorKind::Transport,
        ),
        (
            SqlDispatch::NotDispatched,
            None,
            SqlDispatch::NotDispatched,
            WarehouseErrorKind::Transport,
        ),
        (
            SqlDispatch::Unknown,
            Some(partial.clone()),
            SqlDispatch::Unknown,
            WarehouseErrorKind::Transport,
        ),
        (
            SqlDispatch::NotDispatched,
            Some(partial.clone()),
            SqlDispatch::Unknown,
            WarehouseErrorKind::Transport,
        ),
        (
            SqlDispatch::Terminal,
            None,
            SqlDispatch::Unknown,
            WarehouseErrorKind::Transport,
        ),
        (
            SqlDispatch::Terminal,
            Some(partial.clone()),
            SqlDispatch::Unknown,
            WarehouseErrorKind::Transport,
        ),
    ] {
        let executor = Executor::new([Step::Failure(SqlExecutionError {
            kind: WarehouseErrorKind::Transport,
            dispatch,
            outcome: evidence.clone(),
        })]);
        let error = client(Arc::new(Credential::default()))
            .copy_into(
                &executor,
                &table(),
                &source(),
                &options(),
                SqlLimits::default(),
                &Control::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected_kind);
        assert_eq!(error.dispatch, expected_dispatch);
        assert_eq!(
            error.mutation_may_have_occurred,
            expected_dispatch != SqlDispatch::NotDispatched
        );
        assert_eq!(error.outcome.as_deref(), evidence.as_ref());
        assert_eq!(executor.records().len(), 1);
    }
}

#[tokio::test]
async fn native_interrupt_or_stream_limit_retains_cause_and_partial_ordered_evidence() {
    let partial = SqlOutcome {
        events: vec![
            set(
                &["duplicate", "duplicate"],
                vec![vec![
                    SqlValue::Integer(i64::MAX),
                    SqlValue::Decimal("1.000000000000000001".into()),
                ]],
            ),
            SqlEvent::Done {
                rows_affected: Some(1),
            },
        ],
        terminal: SqlTerminal::Incomplete,
        decoded_bytes: 128,
    };
    for kind in [
        WarehouseErrorKind::Cancelled,
        WarehouseErrorKind::Deadline,
        WarehouseErrorKind::Limit,
    ] {
        let executor = Executor::new([Step::Failure(SqlExecutionError {
            kind,
            dispatch: SqlDispatch::Unknown,
            outcome: Some(partial.clone()),
        })]);
        let error = client(Arc::new(Credential::default()))
            .copy_into(
                &executor,
                &table(),
                &source(),
                &options(),
                SqlLimits::default(),
                &Control::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.dispatch, SqlDispatch::Unknown);
        assert!(error.mutation_may_have_occurred);
        assert_eq!(error.outcome.as_deref(), Some(&partial));
        assert_eq!(executor.records().len(), 1);
    }
}

#[tokio::test]
async fn credential_failure_is_structured_and_pre_dispatch() {
    let executor = Executor::new([]);
    let credential = Arc::new(Credential {
        action: CredentialAction::Failure,
        ..Credential::default()
    });
    let error = client(credential.clone())
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Credential);
    assert_eq!(error.dispatch, SqlDispatch::NotDispatched);
    assert!(!error.mutation_may_have_occurred);
    assert!(executor.records().is_empty());
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![SQL_SCOPE.to_owned()]]
    );
}

#[tokio::test]
async fn pre_cancel_wins_before_credential_polling_even_if_provider_would_hang() {
    let credential = Arc::new(Credential {
        action: CredentialAction::Pending,
        ..Credential::default()
    });
    let control = Control::default();
    control.cancelled.store(true, Ordering::SeqCst);
    let executor = Executor::new([]);
    let error = client(credential.clone())
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits::default(),
            &control,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Cancelled);
    assert_eq!(error.dispatch, SqlDispatch::NotDispatched);
    assert!(!error.mutation_may_have_occurred);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.records().is_empty());
}

#[tokio::test]
async fn cancellation_and_deadline_interrupt_pending_credentials_or_the_pre_dispatch_boundary() {
    for deadline in [false, true] {
        for return_token in [false, true] {
            let control = Arc::new(Control::default());
            let credential = Arc::new(InterruptingCredential {
                control: control.clone(),
                deadline,
                return_token,
                calls: AtomicUsize::new(0),
            });
            let executor = Executor::new([]);
            let client = WarehouseClient::new(
                credential.clone(),
                WarehouseConnection::new(SERVER, "exact catalog").unwrap(),
            );
            let error = client
                .copy_into(
                    &executor,
                    &table(),
                    &source(),
                    &options(),
                    SqlLimits::default(),
                    control.as_ref(),
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.kind,
                if deadline {
                    WarehouseErrorKind::Deadline
                } else {
                    WarehouseErrorKind::Cancelled
                }
            );
            assert_eq!(error.dispatch, SqlDispatch::NotDispatched);
            assert!(!error.mutation_may_have_occurred);
            assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
            assert!(executor.records().is_empty());
        }
    }
}

#[tokio::test]
async fn cancellation_or_deadline_after_dispatch_drops_local_work_but_is_not_rollback() {
    for deadline in [false, true] {
        let control = Arc::new(Control::default());
        let executor = Executor::new([if deadline {
            Step::Deadline(control.clone())
        } else {
            Step::Cancel(control.clone())
        }]);
        let error = client(Arc::new(Credential::default()))
            .copy_into(
                &executor,
                &table(),
                &source(),
                &options(),
                SqlLimits::default(),
                control.as_ref(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if deadline {
                WarehouseErrorKind::Deadline
            } else {
                WarehouseErrorKind::Cancelled
            }
        );
        assert_eq!(error.dispatch, SqlDispatch::Unknown);
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.records().len(), 1);
        assert_eq!(executor.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn native_cancelled_terminal_is_preserved_without_rollback_or_retry() {
    let expected = SqlOutcome {
        events: vec![SqlEvent::Done {
            rows_affected: Some(17),
        }],
        terminal: SqlTerminal::Cancelled,
        decoded_bytes: 8,
    };
    let executor = Executor::new([Step::Outcome(expected.clone())]);
    let error = client(Arc::new(Credential::default()))
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits::default(),
            &Control::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Cancelled);
    assert_eq!(error.dispatch, SqlDispatch::Terminal);
    assert!(error.mutation_may_have_occurred);
    assert_eq!(error.outcome.as_deref(), Some(&expected));
    assert_eq!(executor.records().len(), 1);
}

#[tokio::test]
async fn request_size_empty_query_and_zero_deadline_fail_before_token_or_dispatch() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::new([]);
    let client = client(credential.clone());
    let control = Control::default();
    for sql in ["", " ", "\0"] {
        let error = client
            .query(&executor, &table(), sql, SqlLimits::default(), &control)
            .await
            .unwrap_err();
        assert_eq!(error.kind, WarehouseErrorKind::InvalidInput);
    }
    let error = client
        .query(
            &executor,
            &table(),
            "SELECT 1;",
            SqlLimits {
                max_statement_bytes: nz(8),
                ..SqlLimits::default()
            },
            &control,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Limit);
    let error = client
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits {
                max_statement_bytes: nz(10),
                ..SqlLimits::default()
            },
            &control,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::Limit);
    let error = client
        .copy_into(
            &executor,
            &table(),
            &source(),
            &options(),
            SqlLimits {
                max_duration: Duration::ZERO,
                ..SqlLimits::default()
            },
            &control,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WarehouseErrorKind::InvalidInput);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.records().is_empty());
}

#[tokio::test]
async fn request_limits_and_absolute_deadline_are_passed_to_executor() {
    let executor = Executor::successful();
    let control = Control::default();
    let limits = SqlLimits {
        max_statement_bytes: nz(50),
        max_result_bytes: nz(99),
        max_events: nz(3),
        max_result_sets: nz(2),
        max_rows: nz(4),
        max_columns: nz(5),
        max_duration: Duration::from_secs(17),
        ..SqlLimits::default()
    };
    client(Arc::new(Credential::default()))
        .query(&executor, &table(), "SELECT 1;", limits, &control)
        .await
        .unwrap();
    let records = executor.records();
    let actual = records[0].limits;
    assert_eq!(actual.max_requests, nz(1));
    assert_eq!(actual.max_statement_bytes, limits.max_statement_bytes);
    assert_eq!(actual.max_result_bytes, limits.max_result_bytes);
    assert_eq!(actual.max_events, limits.max_events);
    assert_eq!(actual.max_result_sets, limits.max_result_sets);
    assert_eq!(actual.max_rows, limits.max_rows);
    assert_eq!(actual.max_columns, limits.max_columns);
    assert_eq!(actual.max_duration, limits.max_duration);
    assert_eq!(
        records[0].deadline,
        control.origin + Duration::from_secs(17)
    );
}

#[tokio::test]
async fn all_result_budgets_are_checked_even_when_executor_reports_success() {
    let base = SqlLimits::default();
    let cases = [
        (
            SqlOutcome {
                decoded_bytes: 9,
                ..completed()
            },
            SqlLimits {
                max_result_bytes: nz(8),
                ..base
            },
        ),
        (
            SqlOutcome {
                events: vec![set(&["x"], vec![vec![SqlValue::Text("oversize".into())]])],
                decoded_bytes: 0, // A dishonest/buggy byte count cannot hide values.
                ..completed()
            },
            SqlLimits {
                max_result_bytes: nz(4),
                ..base
            },
        ),
        (
            SqlOutcome {
                events: vec![set(&["a"], vec![]), set(&["b"], vec![])],
                ..completed()
            },
            SqlLimits {
                max_result_sets: nz(1),
                ..base
            },
        ),
        (
            SqlOutcome {
                events: vec![set(&["a"], vec![]), set(&["b"], vec![])],
                ..completed()
            },
            SqlLimits {
                max_events: nz(1),
                ..base
            },
        ),
        (
            SqlOutcome {
                events: vec![set(&["a", "b"], vec![])],
                ..completed()
            },
            SqlLimits {
                max_columns: nz(1),
                ..base
            },
        ),
        (
            SqlOutcome {
                events: vec![
                    set(&["a"], vec![vec![SqlValue::Null]]),
                    set(&["b"], vec![vec![SqlValue::Null]]),
                ],
                ..completed()
            },
            SqlLimits {
                max_rows: nz(1),
                ..base
            },
        ),
        (
            SqlOutcome {
                events: vec![SqlEvent::Information(diagnostic("large information"))],
                decoded_bytes: 0,
                ..completed()
            },
            SqlLimits {
                max_result_bytes: nz(8),
                ..base
            },
        ),
        (
            SqlOutcome {
                events: vec![set(
                    &["x"],
                    vec![vec![SqlValue::Native {
                        encoding: "large lossless encoding tag".into(),
                        bytes: vec![0; 20],
                    }]],
                )],
                decoded_bytes: 0,
                ..completed()
            },
            SqlLimits {
                max_result_bytes: nz(20),
                ..base
            },
        ),
    ];
    for (outcome, limits) in cases {
        let executor = Executor::new([Step::Outcome(outcome.clone())]);
        let error = client(Arc::new(Credential::default()))
            .copy_into(
                &executor,
                &table(),
                &source(),
                &options(),
                limits,
                &Control::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, WarehouseErrorKind::Limit);
        assert_eq!(error.dispatch, SqlDispatch::Terminal);
        assert!(error.mutation_may_have_occurred);
        assert_eq!(error.outcome.as_deref(), Some(&outcome));
        assert_eq!(executor.records().len(), 1);
    }
}

#[tokio::test]
async fn exact_request_and_result_boundaries_are_inclusive() {
    let expected = SqlOutcome {
        events: vec![SqlEvent::ResultSet(SqlResultSet {
            columns: vec![SqlColumn {
                name: "x".into(),
                native_type: "bigint".into(),
                nullable: Some(false),
            }],
            rows: vec![vec![SqlValue::Integer(i64::MAX)]],
            complete: true,
        })],
        terminal: SqlTerminal::Completed,
        decoded_bytes: 15,
    };
    let executor = Executor::new([Step::Outcome(expected.clone())]);
    let actual = client(Arc::new(Credential::default()))
        .query(
            &executor,
            &table(),
            "SELECT 1;",
            SqlLimits {
                max_statement_bytes: nz(9),
                max_result_bytes: nz(15),
                max_events: nz(1),
                max_result_sets: nz(1),
                max_rows: nz(1),
                max_columns: nz(1),
                ..SqlLimits::default()
            },
            &Control::default(),
        )
        .await
        .unwrap();
    assert_eq!(actual, expected);
}
