// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Certificate-only lifecycle fakes; no native store or cryptographic operations.
#![deny(unnameable_test_items)]

use super::*;
use std::{
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};

#[path = "certificate_vectors.rs"]
mod vectors;

fn selector() -> CertificateSelector {
    CertificateSelector::new(
        CertificateStore::CurrentUserMy,
        [0xab; 32],
        AllowedCertificateProvider::MicrosoftSoftware,
    )
}

struct Release(Sender<()>);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn controlled(pool: Arc<WorkerPool>) -> (CertificateSignOperation, Release, Receiver<()>) {
    let (release, wait) = channel();
    let (entered, observed) = channel();
    let operation = start_with(
        selector(),
        b"synthetic-input".to_vec(),
        pool,
        move |_, _, _| {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            WorkOutcome {
                result: Ok(vec![0x5a; 256]),
                cleanup_confirmed: true,
            }
        },
    )
    .unwrap();
    (operation, Release(release), observed)
}

fn poll(operation: &mut CertificateSignOperation) -> Poll<Result<Vec<u8>, CertificateError>> {
    Pin::new(operation).poll(&mut Context::from_waker(Waker::noop()))
}

fn finish(operation: &mut CertificateSignOperation) -> Result<Vec<u8>, CertificateError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Poll::Ready(result) = poll(operation) {
            return result;
        }
        assert!(Instant::now() < deadline, "fake worker did not finish");
        std::thread::yield_now();
    }
}

fn wait_for_count(pool: &WorkerPool, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.active.load(Ordering::Acquire) != expected {
        assert!(Instant::now() < deadline, "fake teardown did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn selector_is_inert_explicit_and_redacted() {
    let selection = selector();
    assert_eq!(selection.store(), CertificateStore::CurrentUserMy);
    assert_eq!(
        selection.allowed_provider(),
        AllowedCertificateProvider::MicrosoftSoftware
    );
    assert_eq!(selection.certificate_sha256(), [0xab; 32]);
    assert_eq!(format!("{selection:?}"), "CertificateSelector { .. }");
    assert_eq!(
        selection.binding(),
        format!(
            "windows-certificate-store:current-user-my:microsoft-software:{}",
            "ab".repeat(32)
        )
    );
    let different_store = CertificateSelector::new(
        CertificateStore::LocalMachineMy,
        [0xab; 32],
        AllowedCertificateProvider::MicrosoftSoftware,
    );
    assert_ne!(selection.binding(), different_store.binding());
}

#[test]
fn invalid_input_precedes_admission_and_worker_creation() {
    let pool = WorkerPool::new(2);
    for input in [Vec::new(), vec![b'x'; SIGNING_INPUT_LIMIT + 1]] {
        assert_eq!(
            start_with(selector(), input, Arc::clone(&pool), |_, _, _| {
                panic!("invalid input must not create work")
            })
            .unwrap_err(),
            CertificateError::InvalidInput
        );
    }
    assert_eq!(pool.active.load(Ordering::Acquire), 0);
}

#[test]
fn v1_provider_policy_is_exact_software_only_and_requires_readable_nonexportability() {
    let policy = AllowedCertificateProvider::MicrosoftSoftware;
    let software = "Microsoft Software Key Storage Provider";
    assert!(validate_key_policy(policy, software, Some(0), true, true, 2048).is_ok());
    for other in [
        "Microsoft Platform Crypto Provider",
        "Microsoft Smart Card Key Storage Provider",
        "microsoft software key storage provider",
        "",
    ] {
        assert_eq!(
            validate_key_policy(policy, other, Some(0), true, true, 2048),
            Err(CertificateError::ProviderNotAllowed)
        );
    }
    assert_eq!(
        validate_key_policy(policy, software, None, true, true, 2048),
        Err(CertificateError::KeyPolicyUnavailable)
    );
    for export_policy in [1, 2, 4, 8, u32::MAX] {
        assert_eq!(
            validate_key_policy(policy, software, Some(export_policy), true, true, 2048),
            Err(CertificateError::ExportableKey)
        );
    }
    for (can_sign, is_rsa, bits) in [
        (false, true, 2048),
        (true, false, 2048),
        (true, true, 1024),
        (true, true, 8192),
    ] {
        assert_eq!(
            validate_key_policy(policy, software, Some(0), can_sign, is_rsa, bits),
            Err(CertificateError::UnsupportedKey)
        );
    }
}

#[test]
fn two_certificate_slots_are_independent_and_cancellation_does_not_recycle_them() {
    let pool = WorkerPool::new(2);
    let (mut first, release_first, first_entered) = controlled(Arc::clone(&pool));
    let (mut second, release_second, second_entered) = controlled(Arc::clone(&pool));
    first_entered.recv_timeout(Duration::from_secs(5)).unwrap();
    second_entered.recv_timeout(Duration::from_secs(5)).unwrap();

    first.cancel();
    assert_eq!(
        poll(&mut first),
        Poll::Ready(Err(CertificateError::Cancelled))
    );
    assert_eq!(pool.active.load(Ordering::Acquire), 2);
    assert_eq!(
        start_with(selector(), vec![b'x'], Arc::clone(&pool), |_, _, _| {
            panic!("saturation must not create another worker")
        })
        .unwrap_err(),
        CertificateError::CapacityExhausted {
            active: 2,
            limit: 2
        }
    );

    drop(release_first);
    wait_for_count(&pool, 1);
    drop(release_second);
    assert_eq!(finish(&mut second).unwrap(), vec![0x5a; 256]);
    wait_for_count(&pool, 0);
}

#[test]
fn cancellation_invalidates_a_ready_but_undelivered_signature() {
    let pool = WorkerPool::new(2);
    let (mut operation, release, entered) = controlled(Arc::clone(&pool));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(release);
    wait_for_count(&pool, 0);
    operation.cancel();
    assert_eq!(finish(&mut operation), Err(CertificateError::Cancelled));
}

#[test]
fn dropping_waiter_preserves_capacity_until_actual_fake_teardown() {
    let pool = WorkerPool::new(2);
    let (operation, release, entered) = controlled(Arc::clone(&pool));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(operation);
    assert_eq!(pool.active.load(Ordering::Acquire), 1);
    drop(release);
    wait_for_count(&pool, 0);
}

#[test]
fn unconfirmed_cleanup_quarantines_only_certificate_capacity() {
    let pool = WorkerPool::new(2);
    let mut operation = start_with(selector(), vec![b'x'], Arc::clone(&pool), |_, _, _| {
        WorkOutcome {
            result: Err(CertificateError::CleanupUnconfirmed {
                stage: CertificateStage::ReleaseKey,
                code: -1,
            }),
            cleanup_confirmed: false,
        }
    })
    .unwrap();
    assert_eq!(
        finish(&mut operation),
        Err(CertificateError::CleanupUnconfirmed {
            stage: CertificateStage::ReleaseKey,
            code: -1,
        })
    );
    assert_eq!(pool.active.load(Ordering::Acquire), 1);
}

#[test]
fn success_without_confirmed_cleanup_is_never_delivered_or_recycled() {
    let pool = WorkerPool::new(1);
    let mut operation = start_with(selector(), vec![b'x'], Arc::clone(&pool), |_, _, _| {
        WorkOutcome {
            result: Ok(vec![0x5a; 256]),
            cleanup_confirmed: false,
        }
    })
    .unwrap();
    assert_eq!(finish(&mut operation), Err(CertificateError::WorkerFailed));
    assert_eq!(
        start_with(selector(), vec![b'x'], Arc::clone(&pool), |_, _, _| {
            panic!("uncertain native capacity must not be reused")
        })
        .unwrap_err(),
        CertificateError::CapacityExhausted {
            active: 1,
            limit: 1
        }
    );
}

#[test]
fn native_failure_categories_do_not_retain_input_or_source_text() {
    use std::error::Error as _;

    for error in [
        CertificateError::ProviderNotAllowed,
        CertificateError::KeyPolicyUnavailable,
        CertificateError::InteractionRequired,
        CertificateError::Native {
            stage: CertificateStage::Sign,
            code: -1,
        },
    ] {
        assert!(error.source().is_none());
        assert!(!format!("{error:?} {error}").contains("synthetic-input"));
    }
}

fn public_hex(value: &str) -> Vec<u8> {
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty());
    pairs
        .iter()
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16).unwrap();
            let low = char::from(pair[1]).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

#[test]
fn published_ps256_vector_verifies_sha256_mgf1_sha256_salt32_not_salt20() {
    let modulus = public_hex(vectors::MODULUS);
    let exponent = public_hex(vectors::EXPONENT);
    let mut signature = public_hex(vectors::SIGNATURE);
    assert!(
        crate::native::certificate::verify_public_vector(
            &modulus,
            &exponent,
            vectors::MESSAGE,
            &signature,
            32,
        )
        .is_ok()
    );
    assert_eq!(
        crate::native::certificate::verify_public_vector(
            &modulus,
            &exponent,
            vectors::MESSAGE,
            &signature,
            20,
        ),
        Err(CertificateError::SignatureInvalid)
    );
    signature[0] ^= 1;
    assert_eq!(
        crate::native::certificate::verify_public_vector(
            &modulus,
            &exponent,
            vectors::MESSAGE,
            &signature,
            32,
        ),
        Err(CertificateError::SignatureInvalid)
    );
}
