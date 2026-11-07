// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Negative, cancellation and output-boundary tests for the native SDK adapter.

#![deny(unnameable_test_items)]

use super::*;
use std::{
    future::{poll_fn, ready},
    task::Poll,
};

#[tokio::test]
async fn native_unknown_type_generation_facet_identity_and_options_fail_before_dispatch() {
    for case in 0..10 {
        let executor = Arc::new(Executor::default());
        let mut host = host(executor.clone(), None);
        let mut args = args(OntologyVerb::Get);
        let original = target(OntologyVerb::Get);
        let mut selection = original.selection().clone();
        let mut child = original.child().cloned();
        match case {
            0 => selection.owner_type = "SemanticModel".into(),
            1 => selection.generation = None,
            2 => selection.generation = Some(1),
            3 => selection.generation = Some(99),
            4 => selection.facet = "docs".into(),
            5 => selection.representation = "expanded".into(),
            6 => child.as_mut().unwrap().kind = "entity.name".into(),
            7 => args.wait = true,
            8 => args.if_match = Some("unsupported".into()),
            9 => args.project = Some("name".into()),
            _ => unreachable!(),
        }
        let selected = ResolvedTarget::new(
            PATH,
            ItemIdentity::new(WORKSPACE, ITEM).unwrap(),
            selection,
            child,
        )
        .unwrap();
        assert!(
            run(&args, true, &registry(), &mut host, selected, limits())
                .await
                .is_err()
        );
        assert!(executor.seen.lock().unwrap().is_empty());
        assert!(host.approved.is_empty());
    }
}

#[tokio::test]
async fn native_byte_limits_are_exact_and_no_source_or_records_are_clipped_as_success() {
    let data = json!({"name":"Machine","lineageTag":LINEAGE});
    let size = serde_json::to_vec(&data).unwrap().len();
    for maximum in [size, size - 1] {
        let executor = Arc::new(Executor::new(vec![response(200, data.clone())]));
        let mut host = host(executor.clone(), None);
        let mut limits = limits();
        limits.data_bytes = NonZeroUsize::new(maximum).unwrap();
        let report = run(
            &args(OntologyVerb::Get),
            true,
            &registry(),
            &mut host,
            target(OntologyVerb::Get),
            limits,
        )
        .await
        .unwrap();
        assert_eq!(report.command_succeeded(), maximum == size);
        assert_eq!(executor.seen.lock().unwrap().len(), 1);
        if maximum == size {
            let receipt: Value = serde_json::from_slice(&host.output.lock().unwrap()).unwrap();
            assert_eq!(receipt["data"], data);
        } else {
            assert!(host.output.lock().unwrap().is_empty());
            assert_eq!(report.unpresented_data, Some(data.clone()));
            let receipt: Value = serde_json::from_slice(&host.stderr).unwrap();
            assert_eq!(receipt["error"]["code"], "BudgetExceeded");
        }
    }
    let body = json!({"name":"Machine"});
    let size = serde_json::to_vec(&body).unwrap().len();
    for maximum in [size, size - 1] {
        let executor = Arc::new(Executor::new(vec![response(201, data.clone())]));
        let mut host = host(executor.clone(), Some(body.clone()));
        let mut limits = limits();
        limits.input_bytes = NonZeroUsize::new(maximum).unwrap();
        let result = run(
            &args(OntologyVerb::Create),
            true,
            &registry(),
            &mut host,
            target(OntologyVerb::Create),
            limits,
        )
        .await;
        if maximum == size {
            assert!(result.unwrap().command_succeeded());
        } else {
            assert!(result.is_err());
            assert!(host.approved.is_empty());
        }
        assert_eq!(
            executor.seen.lock().unwrap().len(),
            usize::from(maximum == size)
        );
    }
}

#[tokio::test]
async fn native_writer_failure_retains_complete_mutation_receipt_without_replay() {
    let executor = Arc::new(Executor::new(vec![response(
        200,
        json!({"name":"Renamed","lineageTag":LINEAGE}),
    )]));
    let mut host = host(executor.clone(), Some(json!({"name":"Renamed"})));
    host.fail_writer = true;
    let result = run(
        &args(OntologyVerb::Move),
        true,
        &registry(),
        &mut host,
        target(OntologyVerb::Move),
        limits(),
    )
    .await;
    let Err(CapabilityError::Output { report, .. }) = result else {
        panic!("original output failure report required");
    };
    assert!(matches!(
        report.receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    assert_eq!(report.receipt.evidence().http_status, Some(200));
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(report.receipt.target().child().unwrap().id, LINEAGE);
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_cancel_before_dispatch_is_not_sent_and_inflight_cancel_is_unknown_without_replay() {
    let executor = Arc::new(Executor::default());
    let mut before_host = host(executor.clone(), Some(json!({"name":"Renamed"})));
    before_host.target = Some(target(OntologyVerb::Move));
    let report = execute_native(
        &args(OntologyVerb::Move),
        true,
        &registry(),
        &mut before_host,
        limits(),
        ready(()),
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Failed { .. }));
    let Some(CapabilityCause::Native(error)) = report.cause else {
        panic!("cancellation evidence required");
    };
    assert_eq!(error.dispatch, DispatchState::NotSent);
    assert!(executor.seen.lock().unwrap().is_empty());
    assert!(!report.receipt.evidence().mutation_may_have_occurred);

    let executor = Arc::new(Executor {
        pending: true,
        ..Executor::default()
    });
    let mut host = host(executor.clone(), Some(json!({"name":"Renamed"})));
    host.target = Some(target(OntologyVerb::Move));
    let observed = executor.clone();
    let cancelled = poll_fn(move |context| {
        if !observed.seen.lock().unwrap().is_empty() {
            Poll::Ready(())
        } else {
            context.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let report = execute_native(
        &args(OntologyVerb::Move),
        true,
        &registry(),
        &mut host,
        limits(),
        cancelled,
    )
    .await
    .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    let Some(CapabilityCause::Native(error)) = report.cause else {
        panic!("inflight evidence required");
    };
    assert_eq!(error.dispatch, DispatchState::Unknown);
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
}

#[test]
fn native_binding_is_transactional_and_never_registers_a_second_descriptor_or_alias() {
    let mut empty = Registry::default();
    assert!(bind_native_operations(&mut empty).is_err());
    assert!(empty.bindings().is_empty());
    let mut registry = registry();
    let bindings = registry.bindings().to_vec();
    assert!(bind_native_operations(&mut registry).is_err());
    assert_eq!(registry.bindings(), bindings);
    assert_eq!(registry.operations().count(), 7);
}

#[tokio::test]
async fn native_real_file_reservation_is_no_clobber_before_dispatch_and_writes_one_receipt() {
    use std::{fs, path::PathBuf, sync::atomic::AtomicUsize};
    struct OutputFile(PathBuf);
    impl Drop for OutputFile {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_file(&self.0).unwrap();
            }
            fs::remove_dir(self.0.parent().unwrap()).unwrap();
        }
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "fabric-native-output-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir(&directory).unwrap();
    let output = OutputFile(directory.join("receipt.json"));
    fs::write(&output.0, b"original-fixture").unwrap();
    let executor = Arc::new(Executor::new(vec![response(
        200,
        json!({"name":"Renamed","lineageTag":LINEAGE}),
    )]));
    let mut invocation = args(OntologyVerb::Move);
    invocation.output = OutputDestination::File(output.0.clone());
    {
        let mut host = host(executor.clone(), Some(json!({"name":"Renamed"})));
        assert!(
            run(
                &invocation,
                true,
                &registry(),
                &mut host,
                target(OntologyVerb::Move),
                limits()
            )
            .await
            .is_err()
        );
        assert!(executor.seen.lock().unwrap().is_empty());
        assert!(host.approved.is_empty());
        assert_eq!(fs::read(&output.0).unwrap(), b"original-fixture");
    }
    fs::remove_file(&output.0).unwrap();
    let mut host = host(executor.clone(), Some(json!({"name":"Renamed"})));
    let report = run(
        &invocation,
        true,
        &registry(),
        &mut host,
        target(OntologyVerb::Move),
        limits(),
    )
    .await
    .unwrap();
    assert!(report.command_succeeded());
    assert!(host.output.lock().unwrap().is_empty());
    let receipt: Value = serde_json::from_slice(&fs::read(&output.0).unwrap()).unwrap();
    assert_eq!(receipt["status"], "succeeded");
    assert_eq!(receipt["data"]["lineageTag"], LINEAGE);
    assert!(host.approved[0].effects().contains(&Effect::LocalWrite));
    assert_eq!(executor.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_completed_response_failure_preserves_possible_mutation_separately_from_uncertainty()
{
    for (verb, dispatch, kind, status, unknown, possible_mutation) in [
        (
            OntologyVerb::Move,
            DispatchState::Completed,
            WorkloadErrorKind::Limit,
            200,
            true,
            true,
        ),
        (
            OntologyVerb::Move,
            DispatchState::Completed,
            WorkloadErrorKind::Protocol("invalid native result"),
            200,
            true,
            true,
        ),
        (
            OntologyVerb::Move,
            DispatchState::NotSent,
            WorkloadErrorKind::Execution,
            503,
            false,
            false,
        ),
        (
            OntologyVerb::Get,
            DispatchState::Completed,
            WorkloadErrorKind::Limit,
            200,
            false,
            false,
        ),
        (
            OntologyVerb::Move,
            DispatchState::Completed,
            WorkloadErrorKind::Http,
            403,
            false,
            true,
        ),
    ] {
        let error = WorkloadError::with_response(
            kind,
            response(status, json!({"nativeEvidence":"original"})),
            dispatch,
        );
        let executor = Arc::new(Executor::failing(error.clone()));
        let mut host = host(
            executor.clone(),
            if verb == OntologyVerb::Move {
                Some(json!({"name":"Renamed"}))
            } else {
                None
            },
        );
        let report = run(
            &args(verb),
            true,
            &registry(),
            &mut host,
            target(verb),
            limits(),
        )
        .await
        .unwrap();
        assert_eq!(
            matches!(report.receipt.outcome(), Outcome::Unknown { .. }),
            unknown
        );
        assert_eq!(
            matches!(report.receipt.outcome(), Outcome::Failed { .. }),
            !unknown
        );
        assert_eq!(
            report.receipt.evidence().mutation_may_have_occurred,
            possible_mutation
        );
        assert_eq!(report.receipt.evidence().http_status, Some(status));
        assert!(!report.command_succeeded());
        assert!(host.output.lock().unwrap().is_empty());
        let Some(CapabilityCause::Native(retained)) = report.cause else {
            panic!("original native evidence required");
        };
        assert_eq!(*retained, error);
        let receipt: Value = serde_json::from_slice(&host.stderr).unwrap();
        assert_eq!(
            receipt["status"],
            if unknown { "unknown" } else { "failed" }
        );
        assert_eq!(executor.seen.lock().unwrap().len(), 1);
    }
}
