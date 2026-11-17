// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual native provider composition using the host's existing SDK, HTTP and time stack.

use super::{
    QueryHostError, QueryPolicy,
    connection::{QueryConnection, QueryConnectionError},
    resolution::{self, QueryResolutionError},
    result::QueryDelivery,
};
use crate::{
    get_budget::GetReadBudget,
    native_tabular::{NativeCommand, Operation},
    tabular_export::ExportSink,
};
use azure_core::http::HttpClient;
use fabric_cli_query_commands::{InputReader, PreparationError, ResolvedRoute};
use fabric_sdk::{Client, runtime::NativeDeadline};
use fabric_sdk_aihub_query::{AiHubQueryClient, Limits as AiHubLimits};
use fabric_sdk_http::{ExecutionTimer, HttpExecutor};
use fabric_sdk_query::{KustoCredentialContext, KustoQueryExecutor, QueryError, QueryLimits};
use fabric_sdk_query_tabular::{ConversionControl, ConversionLimits};
use fabric_sdk_tabular::OutputLimits;
use serde_json::{Map, Value};
use std::{
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// This actual native adapter advertises KQL, never a provider or template selector.
pub const KQL_ENGINE: &str = "kql";

const METADATA_BYTES: usize = 4 * 1024 * 1024;
const RESULT_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
#[path = "query_host_native_tests.rs"]
mod tests;

/// Original command budget and caller-owned cancellation, not a second clock.
pub struct QueryControl<'a> {
    /// Same native deadline used for query credentials, submission and body reads.
    pub deadline: &'a NativeDeadline,
    /// Original host absolute deadline, shared with strict typed conversion.
    pub end: Instant,
    /// Shared local cancellation latch, independent of remote query status.
    pub canceled: &'a AtomicBool,
    /// Host-generated correlation identifier; never query text or an input path.
    pub request_id: &'a str,
}

/// Existing selected native dependencies; construction starts no identity or I/O.
pub struct NativeQueryHost {
    client: Client,
    metadata: HttpExecutor,
    http: Arc<dyn HttpClient>,
    timer: Arc<dyn ExecutionTimer>,
    settings: Option<Map<String, Value>>,
}

/// Native composition failure before a query observation becomes available.
#[derive(Debug, thiserror::Error)]
pub enum NativeQueryError {
    /// Only actual root Query enters this host.
    #[error("The native query host requires a root query invocation")]
    WrongOperation,
    /// Actual metadata resolution/permission/continuation evidence is retained.
    #[error(transparent)]
    Resolution(#[from] QueryResolutionError),
    /// The optional connection did not match the existing selected trusted service.
    #[error(transparent)]
    Connection(#[from] QueryConnectionError),
    /// Missing optional AIHub configuration does not install a default provider.
    #[error("This DataAgent query requires the documented explicit trusted query_connection")]
    AiHubNotConfigured,
    /// Actual SDK or prepared-root policy failed without another provider attempt.
    #[error(transparent)]
    Host(#[from] QueryHostError),
    /// Production KQL selection/read-policy binding failed before query credentials.
    #[error("The exact native KQL credential/read-policy binding is invalid")]
    KqlBinding(#[source] Box<QueryError>),
    /// Actual AIHub client limits/binding failed before query credentials.
    #[error("The exact trusted AIHub client binding is invalid")]
    AiHubBinding(#[source] fabric_sdk_aihub_query::QueryError),
    /// Local observation stopped before a query SDK could be dispatched.
    #[error("Query observation was canceled locally; no query was dispatched")]
    Canceled,
    /// The original invocation budget expired before query dispatch.
    #[error("The original query deadline elapsed before query dispatch")]
    Deadline,
}

impl NativeQueryHost {
    /// Reuse an existing native SDK context, metadata executor and service clients.
    ///
    /// No environment, credential discovery, login, runtime or global settings
    /// access occurs here. The binary alone reads its existing non-secret store.
    pub fn new(
        client: Client,
        metadata: HttpExecutor,
        http: Arc<dyn HttpClient>,
        timer: Arc<dyn ExecutionTimer>,
        settings: Option<Map<String, Value>>,
    ) -> Self {
        Self {
            client,
            metadata,
            http,
            timer,
            settings,
        }
    }

    /// Resolve authoritative bindings, invoke one real native SDK and deliver rich data.
    ///
    /// The fixed 30-second KQL server policy leaves real metadata/credential/open
    /// margin within the existing native host's overall command budget. It is
    /// never replaced with a fresh remaining budget or silently clamped.
    pub async fn dispatch(
        &self,
        command: &NativeCommand,
        control: QueryControl<'_>,
        input: &mut dyn InputReader,
        output: &mut impl ExportSink,
        canceled: impl Future<Output = ()> + Send,
    ) -> Result<QueryDelivery, NativeQueryError> {
        let Operation::Query(options) = &command.operation else {
            return Err(NativeQueryError::WrongOperation);
        };
        if options.parameters.is_some() {
            return Err(
                QueryHostError::Preparation(PreparationError::UnsupportedParameters).into(),
            );
        }
        if control.canceled.load(Ordering::SeqCst) {
            return Err(NativeQueryError::Canceled);
        }
        control
            .deadline
            .remaining()
            .map_err(|_| NativeQueryError::Deadline)?;
        let parsed = resolution::parse_owner(&command.path)?;
        if matches!(&parsed,crate::get_target::GetTarget::Item {
            item_type:Some(kind),..
        } if kind=="KQLDatabase")
            && options
                .engine
                .as_deref()
                .is_some_and(|engine| engine != KQL_ENGINE)
        {
            return Err(QueryHostError::Preparation(PreparationError::UnsupportedEngine).into());
        }
        let configured_connection = if matches!(&parsed,crate::get_target::GetTarget::Item {
            item_type:Some(kind),..
        } if kind=="DataAgent")
        {
            if options.engine.is_some() {
                return Err(
                    QueryHostError::Preparation(PreparationError::UnsupportedEngine).into(),
                );
            }
            let connection = QueryConnection::from_settings(
                self.settings.as_ref(),
                self.client.context().endpoint(),
            )?
            .ok_or(NativeQueryError::AiHubNotConfigured)?;
            let auth = connection.auth_binding(self.client.context())?;
            Some((connection, auth))
        } else {
            None
        };
        let mut canceled = pin!(async {
            canceled.await;
            control.canceled.store(true, Ordering::SeqCst);
        });
        let preparation = async {
            let metadata_budget = GetReadBudget::new(
                &self.metadata,
                &self.metadata,
                &self.metadata,
                None,
                METADATA_BYTES,
                32,
            );
            let metadata_bound =
                NonZeroUsize::new(METADATA_BYTES).expect("positive finite metadata bound");
            let owner = resolution::resolve_owner(
                &command.path,
                &self.client,
                &metadata_budget,
                metadata_bound,
                std::future::pending(),
            )
            .await?;
            let (bound, aihub, kql) = if owner.owner_type == "KQLDatabase" {
                let bound = resolution::resolve_kql(
                    owner,
                    &self.client,
                    metadata_budget,
                    KQL_ENGINE,
                    "1",
                    metadata_bound,
                    std::future::pending(),
                )
                .await?;
                let ResolvedRoute::Kql(route) = bound
                    .target
                    .resolve(options.engine.as_deref())
                    .map_err(QueryHostError::Preparation)?
                else {
                    return Err(QueryHostError::Preparation(PreparationError::InvalidTarget).into());
                };
                let credentials = KustoCredentialContext::new(
                    self.client.credential().clone(),
                    self.client.selection().account(),
                    self.client.selection().tenant(),
                    route.binding.audience_scope(),
                )
                .map_err(|error| NativeQueryError::KqlBinding(Box::new(error)))?;
                let executor = KustoQueryExecutor::new(
                    route.binding,
                    credentials,
                    self.http.clone(),
                    self.timer.clone(),
                )
                .map_err(|error| NativeQueryError::KqlBinding(Box::new(error)))?;
                (bound, None, Some(executor))
            } else {
                let (connection, auth) = match configured_connection {
                    Some(configured) => configured,
                    None => {
                        let connection = QueryConnection::from_settings(
                            self.settings.as_ref(),
                            self.client.context().endpoint(),
                        )?
                        .ok_or(NativeQueryError::AiHubNotConfigured)?;
                        let auth = connection.auth_binding(self.client.context())?;
                        (connection, auth)
                    }
                };
                let bound = resolution::resolve_data_agent(
                    owner,
                    &connection,
                    &self.client,
                    &metadata_budget,
                    metadata_bound,
                    std::future::pending(),
                )
                .await?;
                bound
                    .target
                    .resolve(options.engine.as_deref())
                    .map_err(QueryHostError::Preparation)?;
                let client = AiHubQueryClient::new(
                    auth,
                    self.client.credential().clone(),
                    self.http.clone(),
                    self.timer.clone(),
                    AiHubLimits::default(),
                )
                .map_err(NativeQueryError::AiHubBinding)?;
                (bound, Some(client), None)
            };
            Ok::<_, NativeQueryError>((bound, aihub, kql))
        };
        let (bound, aihub, kql) = self
            .before_query(control.deadline, canceled.as_mut(), preparation)
            .await?;
        let prepared = super::prepare(
            command,
            bound,
            super::Providers {
                aihub: aihub.as_ref(),
                kql: kql
                    .as_ref()
                    .map(|executor| -> &dyn fabric_sdk_query::EngineExecutor { executor }),
            },
            QueryPolicy {
                request_id: control.request_id.into(),
                server_timeout: Duration::from_secs(30),
                kql_limits: QueryLimits::new(
                    4 * 1024 * 1024,
                    RESULT_BYTES,
                    10_000,
                    32,
                    1_024,
                    4_096,
                )
                .expect("fixed finite native query limits"),
                aihub_max_rows: 1_000,
            },
            input,
            output,
        )?;
        let observed = prepared.execute(control.deadline, canceled.as_mut()).await;
        Ok(observed.finish(
            ConversionLimits::new(32, 10_000, 1_024, RESULT_BYTES, 64, 32)
                .expect("fixed finite conversion limits"),
            &ConversionControl::new(control.end, control.canceled),
            OutputLimits::new(10_000, RESULT_BYTES, 64).expect("fixed finite output limits"),
            control.deadline,
            control.canceled,
            output,
        ))
    }

    async fn before_query<T>(
        &self,
        deadline: &NativeDeadline,
        mut canceled: Pin<&mut (impl Future<Output = ()> + Send)>,
        operation: impl Future<Output = Result<T, NativeQueryError>>,
    ) -> Result<T, NativeQueryError> {
        let remaining = deadline
            .remaining()
            .map_err(|_| NativeQueryError::Deadline)?;
        let mut timeout = self.timer.sleep(remaining);
        let mut operation = pin!(operation);
        poll_fn(|context| {
            if canceled.as_mut().poll(context).is_ready() {
                return std::task::Poll::Ready(Err(NativeQueryError::Canceled));
            }
            if timeout.as_mut().poll(context).is_ready() || deadline.remaining().is_err() {
                return std::task::Poll::Ready(Err(NativeQueryError::Deadline));
            }
            let result = operation.as_mut().poll(context);
            if canceled.as_mut().poll(context).is_ready() {
                return std::task::Poll::Ready(Err(NativeQueryError::Canceled));
            }
            if timeout.as_mut().poll(context).is_ready() || deadline.remaining().is_err() {
                return std::task::Poll::Ready(Err(NativeQueryError::Deadline));
            }
            result
        })
        .await
    }
}
