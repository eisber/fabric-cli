// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_output::CommandError;
use fabric_sdk::{
    capacities::CapacityListErrorKind,
    items::{ItemErrorKind, ItemExecutionError},
    models::FabricServiceError,
    workspaces::{WorkspaceExecutionError, WorkspaceListErrorKind},
};

pub(crate) fn error(code: &str, message: impl Into<String>) -> CommandError {
    CommandError::new(Some(message.into()), Some(code.into()))
}

pub(crate) fn unsupported(message: &str) -> CommandError {
    error("NotSupported", message)
}

pub(crate) fn http(status: u16, code: &str, service: Option<&FabricServiceError>) -> CommandError {
    let message = match status {
        401 => "Access is unauthorized",
        403 => "Access is forbidden. You do not have permission to access this resource",
        404 => "The requested resource could not be found",
        _ => {
            return error(
                code,
                service.map_or("An error occurred while processing the operation", |e| {
                    &e.message
                }),
            );
        }
    };
    error(code, message)
}

fn cancelled() -> CommandError {
    error("LongRunningOperationCancelled", "Operation cancelled")
}

pub(crate) fn workspace(kind: &WorkspaceListErrorKind) -> CommandError {
    match kind {
        WorkspaceListErrorKind::Http {
            status,
            code,
            service_error,
        } => http(*status, code, service_error.as_deref()),
        WorkspaceListErrorKind::Cancelled
        | WorkspaceListErrorKind::Execution(WorkspaceExecutionError::Cancelled) => cancelled(),
        WorkspaceListErrorKind::Credential => error("Unauthorized", "Access is unauthorized"),
        WorkspaceListErrorKind::InvalidResponse => error(
            "InvalidResponse",
            "Fabric returned an invalid workspace-list page",
        ),
        _ => error("ReadFailed", "Workspace listing did not complete"),
    }
}

pub(crate) fn item(kind: &ItemErrorKind) -> CommandError {
    match kind {
        ItemErrorKind::Http {
            status,
            code,
            service_error,
            ..
        } => http(*status, code, service_error.as_deref()),
        ItemErrorKind::Cancelled | ItemErrorKind::Execution(ItemExecutionError::Cancelled) => {
            cancelled()
        }
        ItemErrorKind::Credential => error("Unauthorized", "Access is unauthorized"),
        ItemErrorKind::InvalidResponse => error(
            "InvalidResponse",
            "Fabric returned an invalid item response",
        ),
        ItemErrorKind::InvalidIdentifier => {
            error("InvalidInput", "Invalid item request identifier")
        }
        _ => error("ReadFailed", "Item operation did not complete"),
    }
}

pub(crate) fn capacity(kind: &CapacityListErrorKind) -> CommandError {
    match kind {
        CapacityListErrorKind::Http {
            status,
            code,
            service_error,
        } => http(*status, code, service_error.as_deref()),
        CapacityListErrorKind::Cancelled
        | CapacityListErrorKind::Execution(WorkspaceExecutionError::Cancelled) => cancelled(),
        CapacityListErrorKind::Credential => error("Unauthorized", "Access is unauthorized"),
        CapacityListErrorKind::InvalidResponse => error(
            "InvalidResponse",
            "Fabric returned an invalid capacity-list page",
        ),
        _ => error("ReadFailed", "Capacity listing did not complete"),
    }
}
