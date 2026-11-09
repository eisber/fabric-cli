// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

/// A bodyless ARM resource GET, never an arbitrary URL or a Fabric operation.
///
/// The additive ARM session accepts public `management.azure.com` only. Fabric
/// constructors, completion policies and their existing endpoint rules remain
/// unchanged.
pub struct ArmGetOperation {
    path: String,
}

impl ArmGetOperation {
    /// Validate a subscription/resource route with its explicit API-version query.
    ///
    /// # Errors
    /// Rejects authorities, traversal, controls, fragments, oversized URLs and
    /// routes outside `/subscriptions`. No credential or executor is consulted.
    pub fn new(path: &str) -> Result<Self, OperationErrorKind> {
        if path.len() > 16 * 1024 {
            return Err(OperationErrorKind::InvalidInput);
        }
        let endpoint = EndpointBinding::public(ServiceOrigin::Arm);
        let url = endpoint
            .url(path)
            .map_err(|_| OperationErrorKind::InvalidInput)?;
        if !(url.path() == "/subscriptions" || url.path().starts_with("/subscriptions/")) {
            return Err(OperationErrorKind::InvalidInput);
        }
        Ok(Self { path: path.into() })
    }
}

impl<'a, E: OperationExecutor> OperationSession<'a, E> {
    /// Select the fixed public ARM origin for explicit read-only reconciliation.
    ///
    /// This does not discover credentials or authorize an ARM request using a
    /// Fabric selection. SDK callers must inject an explicitly ARM-bound selection.
    pub fn for_arm(
        credential: &'a dyn TokenCredential,
        selection: &'a CredentialSelection,
        executor: E,
        limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> Self {
        let mut session = Self::new(credential, selection, executor, limits, cancelled);
        session.endpoint = EndpointBinding::public(ServiceOrigin::Arm);
        session
    }

    /// Execute an ARM GET and reconcile source 201/202 through the shared poller.
    ///
    /// `Azure-AsyncOperation` takes precedence over `Location`. The former checks
    /// JSON terminal status; the latter waits for HTTP 200 without interpreting
    /// a status body. Both return the original response bytes with status 200.
    /// ARM never fetches a Fabric-style `/result`, even if terminal Location is set.
    ///
    /// # Errors
    /// Retains typed cancellation, bounds, service data and accepted operation IDs.
    /// GET never sets mutation uncertainty. A used session cannot reset budgets.
    pub async fn execute_arm_get(
        &mut self,
        operation: ArmGetOperation,
    ) -> Result<OperationResponse, OperationError> {
        let result = async {
            self.validate_arm()?;
            if self.requests != 0 {
                return Err(OperationErrorKind::InvalidInput);
            }
            let response = self
                .request(
                    ResourceMethod::Get,
                    service_url(&self.endpoint, &operation.path)?,
                    Vec::new(),
                    false,
                )
                .await?;
            if matches!(response.status, 201 | 202) {
                self.resolve_arm_get(response).await
            } else {
                ensure_source_status(&response, &[200, 204], true)?;
                Ok(response)
            }
        }
        .await;
        result.map_err(|kind| self.evidence(kind))
    }

    /// Reconcile an already-received ARM GET acceptance without replaying it.
    ///
    /// # Errors
    /// Requires a fresh explicit ARM session and actual 201/202 input. The initial
    /// response counts against the same finite request and response-byte budgets.
    pub async fn reconcile_arm_get(
        &mut self,
        response: OperationResponse,
    ) -> Result<OperationResponse, OperationError> {
        let result = async {
            self.validate_arm()?;
            if self.requests != 0 || !matches!(response.status, 201 | 202) {
                return Err(OperationErrorKind::InvalidInput);
            }
            self.requests = 1;
            self.resolve_arm_get(response).await
        }
        .await;
        result.map_err(|kind| self.evidence(kind))
    }

    fn validate_arm(&self) -> Result<(), OperationErrorKind> {
        if self.endpoint != EndpointBinding::public(ServiceOrigin::Arm) {
            return Err(OperationErrorKind::InvalidInput);
        }
        if self.selection.audience() != Audience::Arm {
            return Err(OperationErrorKind::InvalidAudience);
        }
        Ok(())
    }

    async fn resolve_arm_get(
        &mut self,
        response: OperationResponse,
    ) -> Result<OperationResponse, OperationErrorKind> {
        self.operation_id = operation_id(&response)?;
        self.check_cancelled().await.map_err(|error| error.kind)?;
        if response.body.len() > self.limits.max_response_bytes.get() {
            return Err(OperationErrorKind::Limit);
        }
        let asynchronous = header(&response, "Azure-AsyncOperation")?;
        let check_status = asynchronous.is_some();
        let uri = match asynchronous {
            Some(uri) => uri,
            None => header(&response, "Location")?.ok_or(OperationErrorKind::InvalidResponse)?,
        };
        let path = polling_path(uri)?;
        let original = Some(response.clone());
        self.poll_operation(
            response,
            &path,
            CompletionPolicy::PythonSynchronous,
            original,
            Some(check_status),
        )
        .await
    }
}

fn polling_path(uri: &str) -> Result<String, OperationErrorKind> {
    let path = uri
        .strip_prefix("https://management.azure.com:443")
        .or_else(|| uri.strip_prefix("https://management.azure.com"))
        .ok_or(OperationErrorKind::InvalidLink)?;
    if uri.len() > 16 * 1024 || !path.starts_with('/') {
        return Err(OperationErrorKind::InvalidLink);
    }
    let endpoint = EndpointBinding::public(ServiceOrigin::Arm);
    endpoint
        .url(path)
        .map_err(|_| OperationErrorKind::InvalidLink)?;
    Ok(path.into())
}

pub(super) fn operation_id(
    response: &OperationResponse,
) -> Result<Option<String>, OperationErrorKind> {
    if let Some(id) = header(response, "x-ms-operation-id")?.filter(|id| valid_source_id(id)) {
        return Ok(Some(id.into()));
    }
    let Some(uri) = header(response, "Azure-AsyncOperation")?.or(header(response, "Location")?)
    else {
        return Ok(None);
    };
    let path = polling_path(uri)?;
    Ok(path
        .split('?')
        .next()
        .and_then(|path| path.rsplit('/').next())
        .filter(|id| valid_source_id(id))
        .map(str::to_owned))
}
