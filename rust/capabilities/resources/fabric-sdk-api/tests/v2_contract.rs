// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public consumer checks for isolated v2 policy and native-observation contracts.

#![deny(unnameable_test_items)]

use fabric_sdk_api::{
    v2_outcome::{
        Coverage, CoverageScope, Effect, NativeResponse, OperationReference,
        OperationReferenceKind, Outcome, ResponseRole, RetentionLimits, State,
    },
    v2_policy::{Controls, InvocationBudget, Limits, PolicyError, ReplaySafety},
};
use std::{num::NonZeroUsize, time::Duration};

fn budget(controls: Controls, total_bytes: usize) -> InvocationBudget {
    InvocationBudget::new(
        Limits {
            timeout: Duration::from_secs(10),
            max_sends: NonZeroUsize::new(8).unwrap(),
            max_pages: NonZeroUsize::new(2).unwrap(),
            max_response_bytes: NonZeroUsize::new(4).unwrap(),
            max_total_bytes: NonZeroUsize::new(total_bytes).unwrap(),
            max_retries: 1,
        },
        controls,
        Duration::ZERO,
    )
    .unwrap()
}

fn response(role: ResponseRole, status: u16, body: &[u8]) -> NativeResponse {
    NativeResponse::new(
        role,
        status,
        Vec::new(),
        body.to_vec(),
        RetentionLimits::default(),
    )
    .unwrap()
}

fn outcome() -> Outcome {
    Outcome::new(
        Effect::UnknownWriteCapable,
        Coverage::new(CoverageScope::Response, None, 0).unwrap(),
        RetentionLimits::default(),
    )
}

#[test]
fn accepted_observation_survives_local_cancellation_without_reclassification() {
    let mut budget = budget(Controls::default(), 8);
    let mut outcome = outcome();
    budget.before_send(Duration::from_secs(1)).unwrap();
    outcome
        .append(response(ResponseRole::Initial, 202, b""))
        .unwrap();
    outcome
        .set_operation_reference(Some(
            OperationReference::new(OperationReferenceKind::Operation, "operation-1".into(), 64)
                .unwrap(),
        ))
        .unwrap();
    outcome.set_state(State::Accepted).unwrap();
    budget.cancel();

    assert_eq!(
        budget.before_send(Duration::from_secs(2)),
        Err(PolicyError::Cancelled)
    );
    assert_eq!(budget.progress().sends, 1);
    assert_eq!(outcome.state(), State::Accepted);
    assert_eq!(outcome.responses()[0].status(), 202);
    assert_eq!(outcome.operation_reference().unwrap().id(), "operation-1");
    assert_eq!(outcome.coverage().complete(), None);
    assert_eq!(outcome.effect(), Effect::UnknownWriteCapable);
}

#[test]
fn cumulative_retry_bytes_are_not_replenished_by_outcome_retention() {
    let mut budget = budget(Controls::default(), 6);
    let mut outcome = outcome();
    budget.before_send(Duration::ZERO).unwrap();
    budget.record_chunk(0, 3, Duration::ZERO).unwrap();
    outcome
        .append(response(ResponseRole::Initial, 503, b"err"))
        .unwrap();
    budget
        .retry_allowed(ReplaySafety::DeclaredSafeRead, 0, Duration::ZERO)
        .unwrap();
    budget.before_send(Duration::ZERO).unwrap();

    assert_eq!(
        budget.record_chunk(0, 4, Duration::ZERO),
        Err(PolicyError::TotalByteLimit)
    );
    outcome.set_state(State::Partial).unwrap();
    assert_eq!(budget.progress().decoded_bytes, 7);
    assert_eq!(budget.progress().sends, 2);
    assert_eq!(outcome.responses().len(), 1);
    assert_eq!(outcome.responses()[0].status(), 503);
    assert_eq!(outcome.responses()[0].body(), b"err");
    assert_eq!(outcome.state(), State::Partial);
    assert_eq!(outcome.coverage().complete(), None);
}

#[test]
fn final_page_at_limit_keeps_duplicates_and_scoped_completeness() {
    let mut budget = budget(Controls::default(), 8);
    let mut outcome = outcome();
    for _ in 0..2 {
        budget.before_page(Duration::ZERO).unwrap();
        budget.before_send(Duration::ZERO).unwrap();
        budget.record_chunk(0, 3, Duration::ZERO).unwrap();
        outcome
            .append(response(ResponseRole::Page, 200, b"[1]"))
            .unwrap();
        budget.commit_page(Duration::ZERO).unwrap();
    }
    outcome.set_state(State::Completed).unwrap();
    outcome.set_coverage(Coverage::new(CoverageScope::Collection, Some(true), 2).unwrap());

    assert_eq!(outcome.responses().len(), 2);
    assert_eq!(outcome.responses()[0].body(), b"[1]");
    assert_eq!(outcome.responses()[1].body(), b"[1]");
    assert_eq!(outcome.coverage().complete(), Some(true));
    assert_eq!(budget.check(Duration::ZERO), Ok(()));
    assert_eq!(
        budget.before_page(Duration::ZERO),
        Err(PolicyError::PageLimit)
    );
    assert_eq!(budget.progress().sends, 2);
}

#[test]
fn raw_binary_result_does_not_replace_initial_acceptance_or_effect_metadata() {
    let mut outcome = outcome();
    outcome
        .append(response(ResponseRole::Initial, 202, b""))
        .unwrap();
    outcome.set_state(State::Accepted).unwrap();
    outcome
        .append(response(ResponseRole::Result, 200, &[0, 255, 128]))
        .unwrap();
    outcome.set_state(State::Completed).unwrap();

    assert_eq!(outcome.responses()[0].status(), 202);
    assert_eq!(outcome.responses()[0].body(), b"");
    assert_eq!(outcome.responses()[1].body(), [0, 255, 128]);
    assert_eq!(outcome.state(), State::Completed);
    assert_eq!(outcome.effect(), Effect::UnknownWriteCapable);
    assert_eq!(outcome.coverage().complete(), None);
}

#[test]
fn retry_counter_resets_for_new_exchange_but_shared_send_budget_does_not() {
    let mut budget = budget(Controls::default(), 8);
    for _ in 0..2 {
        budget.before_send(Duration::ZERO).unwrap();
        budget
            .retry_allowed(ReplaySafety::DeclaredSafeRead, 0, Duration::ZERO)
            .unwrap();
        budget.before_send(Duration::ZERO).unwrap();
        assert_eq!(
            budget.retry_allowed(ReplaySafety::DeclaredSafeRead, 1, Duration::ZERO),
            Err(PolicyError::RetryLimit)
        );
    }
    assert_eq!(budget.progress().sends, 4);
    assert_eq!(
        budget.remaining(Duration::from_secs(9)),
        Ok(Duration::from_secs(1))
    );
    assert_eq!(
        budget.check(Duration::from_secs(10)),
        Err(PolicyError::TimedOut)
    );
}
