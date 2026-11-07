// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! SDK failures use the same command mapping for ordinary and exported reads.
#![deny(unnameable_test_items)]

use fabric_cli_jobs::JobCommandError;
use fabric_cli_output::{Indent, Timestamp};
use fabric_sdk_jobs::{JobError, PagedValues, PagingError, PagingErrorKind};
use fabric_sdk_operations::{OperationError, OperationErrorKind};
use serde_json::json;
use std::collections::BTreeMap;

fn fetch_failure(error: JobError) -> JobError {
    JobError::Paging(Box::new(PagingError {
        kind: PagingErrorKind::Fetch(error),
        partial: PagedValues {
            value: vec![json!({"id": "completed-earlier-page"})],
            pages_received: 1,
            next_token: Some("synthetic-next-page".into()),
        },
    }))
}

fn timestamp() -> Timestamp {
    Timestamp::parse("2000-01-01T00:00:00Z").unwrap()
}

#[test]
fn direct_and_nested_page_auth_failures_keep_native_codes_messages_and_exits() {
    for (status, code, exit, message) in [
        (401, "Unauthorized", 4, "Access is unauthorized"),
        (
            403,
            "Forbidden",
            1,
            "Access is forbidden. You do not have permission to access this resource",
        ),
    ] {
        for depth in 0..=2 {
            let mut error = JobError::Http {
                status,
                code: code.into(),
                service_error: None,
                raw_service_error: Some(Box::new(json!({"message": "synthetic provider detail"}))),
                headers: BTreeMap::new(),
            };
            for _ in 0..depth {
                error = fetch_failure(error);
            }
            let mapped = JobCommandError::from(error);
            let envelope = mapped.envelope(timestamp(), Some("job run-list".into()));
            assert_eq!(envelope.result().error_code(), Some(code));
            assert_eq!(envelope.result().message(), Some(message));
            assert_eq!(envelope.render(Indent::Four).unwrap().exit_code, exit);
        }
    }
}

#[test]
fn nested_page_operation_failure_retains_the_original_reconciliation_evidence() {
    let original = OperationError {
        kind: OperationErrorKind::OperationFailed {
            status: "Cancelled".into(),
            error: None,
        },
        operation_id: Some("11111111-1111-1111-1111-111111111111".into()),
        mutation_may_have_occurred: true,
        raw_service_error: Some(Box::new(json!({"detail": ["native", null, 1.0]}))),
    };
    let mapped = JobCommandError::from(fetch_failure(fetch_failure(JobError::Operation(
        original.clone(),
    ))));
    let JobCommandError::Operation(retained) = &mapped else {
        panic!("paging must not flatten original operation evidence")
    };
    assert_eq!(retained.as_ref(), &original);
    let envelope = mapped.envelope(timestamp(), Some("job run-list".into()));
    assert_eq!(
        envelope.result().error_code(),
        Some("LongRunningOperationCancelled")
    );
    assert_eq!(retained.as_ref(), &original);
}

#[test]
fn paging_cancellation_is_not_an_ordinary_job_read_failure() {
    let mapped = JobCommandError::from(JobError::Paging(Box::new(PagingError {
        kind: PagingErrorKind::Cancelled,
        partial: PagedValues {
            value: Vec::new(),
            pages_received: 0,
            next_token: None,
        },
    })));
    assert_eq!(
        mapped.envelope(timestamp(), None).result().error_code(),
        Some("LongRunningOperationCancelled")
    );
}
