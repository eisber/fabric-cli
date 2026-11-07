// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use crate::macos_helper::{
    CHILD_POLL, CLEANUP_WAIT, ChildPermit, HelperError, MAX_DEADLINE, blocking_exchange,
    read_bounded,
};
use std::{
    io::Write,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn der(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut result = vec![tag];
    if value.len() < 128 {
        result.push(value.len() as u8);
    } else if value.len() < 256 {
        result.extend_from_slice(&[0x81, value.len() as u8]);
    } else {
        result.extend_from_slice(&[0x82, (value.len() >> 8) as u8, value.len() as u8]);
    }
    result.extend_from_slice(value);
    result
}

fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

// Public-only DER envelope, following the existing PKCS#11 lane fixture pattern.
// The outer certificate signature is synthetic, not a CA trust/enrollment proof.
fn public_certificate() -> Vec<u8> {
    let rsa = sequence(&[
        der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1]),
        der(0x05, &[]),
    ]);
    let algorithm = sequence(&[
        der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 11]),
        der(0x05, &[]),
    ]);
    let mut modulus = vec![0];
    modulus.extend_from_slice(&hex(public_vector::MODULUS));
    let key = sequence(&[
        der(0x02, &modulus),
        der(0x02, &hex(public_vector::EXPONENT)),
    ]);
    let mut public_bits = vec![0];
    public_bits.extend_from_slice(&key);
    let tbs = sequence(&[
        der(0xa0, &der(0x02, &[2])),
        der(0x02, &[1]),
        algorithm.clone(),
        sequence(&[]),
        sequence(&[der(0x17, b"200101000000Z"), der(0x17, b"491231235959Z")]),
        sequence(&[]),
        sequence(&[rsa, der(0x03, &public_bits)]),
    ]);
    sequence(&[tbs, algorithm, der(0x03, &[0, 0])])
}

fn selector() -> MacCertificateSelector {
    MacCertificateSelector::new("/Library/Keychains/explicit.keychain-db".into(), [7; 32]).unwrap()
}

#[test]
fn selector_is_inert_absolute_exact_and_redacted() {
    let selection = selector();
    assert_eq!(selection.certificate_sha256(), [7; 32]);
    assert_eq!(
        selection.keychain_path(),
        "/Library/Keychains/explicit.keychain-db"
    );
    assert!(!format!("{selection:?}").contains("explicit"));
    for path in [
        "",
        "/",
        "login.keychain-db",
        "~/Library/login.keychain-db",
        "//Library/keychain",
        "/Library/../keychain",
        "/Library/./keychain",
        "/Library//keychain",
        "/Library/keychain/",
        "/Library/keychain\0ignored",
        "/Library/keychain\n",
    ] {
        assert!(MacCertificateSelector::new(path.into(), [7; 32]).is_err());
    }
    assert!(MacCertificateSelector::new(format!("/{}", "a".repeat(4096)), [0; 32]).is_err());
}

#[test]
fn native_policy_is_tristate_without_false_defaults() {
    for can_sign in [None, Some(false), Some(true)] {
        assert_eq!(
            validate_mac_key_policy(None, can_sign),
            Err(MacCertificateError::PolicyUnavailable)
        );
    }
    assert_eq!(
        validate_mac_key_policy(Some(false), None),
        Err(MacCertificateError::PolicyUnavailable)
    );
    assert_eq!(
        validate_mac_key_policy(Some(true), Some(true)),
        Err(MacCertificateError::ExportableKey)
    );
    assert_eq!(
        validate_mac_key_policy(Some(false), Some(false)),
        Err(MacCertificateError::UnsupportedKey)
    );
    assert_eq!(validate_mac_key_policy(Some(false), Some(true)), Ok(()));
}

#[test]
fn requests_are_finite_closed_and_never_argv() {
    let encoded = MacHelperRequest::new(selector(), b"header.payload")
        .unwrap()
        .encode()
        .unwrap();
    let (selected, bytes) = MacHelperRequest::decode(&encoded).unwrap();
    assert_eq!(selected.certificate_sha256(), [7; 32]);
    assert_eq!(bytes, b"header.payload");
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["unexpected"] = true.into();
    assert!(MacHelperRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    value.as_object_mut().unwrap().remove("unexpected");
    value["selector"]["ambientFirstIdentity"] = true.into();
    assert!(MacHelperRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    value["selector"]
        .as_object_mut()
        .unwrap()
        .remove("ambientFirstIdentity");
    value["schema"] = 2.into();
    assert!(MacHelperRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    value["schema"] = 1.into();
    value["signingInput"] = "aA==".into();
    assert!(MacHelperRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    value["signingInput"] = "".into();
    assert!(MacHelperRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut trailing = encoded.clone();
    trailing.extend_from_slice(b"{}");
    assert!(MacHelperRequest::decode(&trailing).is_err());
    let duplicated = format!(
        "{{\"schema\":1,{}",
        std::str::from_utf8(&encoded)
            .unwrap()
            .trim_start_matches('{')
    );
    assert!(MacHelperRequest::decode(duplicated.as_bytes()).is_err());
    assert!(MacHelperRequest::decode(&vec![b' '; MAC_HELPER_WIRE_LIMIT + 1]).is_err());
    assert!(MacHelperRequest::new(selector(), &[]).is_err());
    assert!(MacHelperRequest::new(selector(), &vec![0; SIGNING_INPUT_LIMIT + 1]).is_err());
    assert!(
        MacHelperRequest::new(selector(), &vec![0; SIGNING_INPUT_LIMIT])
            .unwrap()
            .encode()
            .is_err()
    );
}

#[test]
fn replies_reject_unknown_fields_fake_certificates_and_os_diagnostics() {
    let selected = selector();
    let error = MacHelperReply::Error {
        schema: 1,
        code: MacCertificateError::PolicyUnavailable,
    }
    .encode()
    .unwrap();
    assert_eq!(
        MacHelperReply::verify(&error, &selected, b"message"),
        Err(MacCertificateError::PolicyUnavailable)
    );
    for reply in [
        br#"{"status":"error","schema":1,"code":"policy_unavailable","path":"secret"}"#.as_slice(),
        br#"{"status":"error","schema":1,"code":"raw OS description"}"#.as_slice(),
        br#"{"status":"error","schema":2,"code":"selection"}"#.as_slice(),
        br#"{"status":"ok","schema":1,"certificate":"AQ","signature":"AQ"}"#.as_slice(),
        br#"{"status":"ok","schema":1,"certificate":"","signature":""}"#.as_slice(),
        br#"{"status":"ok","schema":1,"certificate":"AQ","signature":"AQ"} {}"#.as_slice(),
    ] {
        assert!(MacHelperReply::verify(reply, &selected, b"message").is_err());
    }
    assert!(
        MacHelperReply::verify(
            &vec![b' '; MAC_HELPER_WIRE_LIMIT + 1],
            &selected,
            b"message"
        )
        .is_err()
    );
}

#[test]
fn public_key_validation_never_accepts_arbitrary_der_or_fingerprint() {
    assert_eq!(
        mac_certificate_rsa_bits(&[], &[0; 32]),
        Err(MacCertificateError::Selection)
    );
    assert_eq!(
        mac_certificate_rsa_bits(&vec![1; MAC_CERTIFICATE_DER_LIMIT + 1], &[0; 32]),
        Err(MacCertificateError::Selection)
    );
    let malformed = b"not a certificate";
    let actual: [u8; 32] = digest(&SHA256, malformed).as_ref().try_into().unwrap();
    assert_eq!(
        mac_certificate_rsa_bits(malformed, &actual),
        Err(MacCertificateError::Selection)
    );
    for malformed in [&[][..], &[0][..], &[0, 0x7f][..], &[0x80][..]] {
        assert!(unsigned_integer(malformed).is_err());
    }
}

#[test]
fn existing_public_pss_vector_verifies_message_not_digest() {
    // Reuse the repository's public-only Wycheproof vector and its existing
    // Apache-2.0 provenance; no fixture/key generation or native API is involved.
    let modulus = hex(public_vector::MODULUS);
    let exponent = hex(public_vector::EXPONENT);
    let signature = hex(public_vector::SIGNATURE);
    let key = CertificatePublicKey {
        bits: 2048,
        modulus,
        exponent,
    };
    assert_eq!(key.verify(public_vector::MESSAGE, &signature), Ok(()));
    assert_eq!(
        key.verify(digest(&SHA256, public_vector::MESSAGE).as_ref(), &signature),
        Err(MacCertificateError::Signature)
    );
    assert_eq!(
        key.verify(b"changed", &signature),
        Err(MacCertificateError::Signature)
    );
    let mut changed = signature.clone();
    changed[0] ^= 1;
    assert_eq!(
        key.verify(public_vector::MESSAGE, &changed),
        Err(MacCertificateError::Signature)
    );
    let mut wrong_key = key;
    wrong_key.modulus[0] ^= 1;
    assert!(
        wrong_key
            .verify(public_vector::MESSAGE, &signature)
            .is_err()
    );
}

#[test]
fn selected_certificate_is_verified_again_before_host_publication() {
    let certificate = public_certificate();
    let expected = digest(&SHA256, &certificate).as_ref().try_into().unwrap();
    let selected =
        MacCertificateSelector::new("/Library/Keychains/exact.keychain-db".into(), expected)
            .unwrap();
    let signature = hex(public_vector::SIGNATURE);
    assert_eq!(mac_certificate_rsa_bits(&certificate, &expected), Ok(2048));
    let reply = MacHelperReply::Ok {
        schema: 1,
        certificate: URL_SAFE_NO_PAD.encode(&certificate),
        signature: URL_SAFE_NO_PAD.encode(&signature),
    }
    .encode()
    .unwrap();
    assert_eq!(
        MacHelperReply::verify(&reply, &selected, public_vector::MESSAGE),
        Ok(signature)
    );
    assert_eq!(
        MacHelperReply::verify(&reply, &selector(), public_vector::MESSAGE),
        Err(MacCertificateError::Selection)
    );
    assert_eq!(
        MacHelperReply::verify(&reply, &selected, b"changed"),
        Err(MacCertificateError::Signature)
    );
    let mut trailing = certificate;
    trailing.push(0);
    let new_hash = digest(&SHA256, &trailing).as_ref().try_into().unwrap();
    assert_eq!(
        mac_certificate_rsa_bits(&trailing, &new_hash),
        Err(MacCertificateError::Selection)
    );
}

use super::super::public_certificate_vectors as public_vector;

#[test]
fn finite_pipe_reads_accept_exact_limit_and_reject_overflow() {
    assert_eq!(
        read_bounded(std::io::Cursor::new(b"12345"), 4),
        Err(HelperError::Helper)
    );
    assert_eq!(
        read_bounded(std::io::Cursor::new(b"1234"), 4).unwrap(),
        b"1234"
    );
}

fn process_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn process_runtime(io: bool) -> tokio::runtime::Runtime {
    let mut builder = tokio::runtime::Builder::new_current_thread();
    builder.enable_time();
    if io {
        builder.enable_io();
    }
    builder.build().unwrap()
}

fn process_fixture(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    let test_module = module_path!().split_once("::").unwrap().1;
    command
        .args([
            "--exact",
            &format!("{test_module}::owned_helper_process_fixture"),
            "--nocapture",
        ])
        .env("FAB_MACOS_TEST_CHILD", mode);
    command
}

// A real test subprocess, never the native helper and never a Keychain lookup.
// Normal test discovery is inert; only an explicit child command selects a case.
#[test]
fn owned_helper_process_fixture() {
    let Ok(mode) = std::env::var("FAB_MACOS_TEST_CHILD") else {
        return;
    };
    match mode.as_str() {
        "hang" => loop {
            thread::sleep(Duration::from_secs(60));
        },
        "oversized" => {
            let _ = std::io::stdout()
                .lock()
                .write_all(&vec![b'x'; MAC_HELPER_WIRE_LIMIT + 1]);
        }
        _ => {}
    }
}

#[test]
fn time_only_runtime_does_not_spawn_the_real_fixture_command() {
    let _serial = process_test_lock();
    let runtime = process_runtime(false);
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let spawned = AtomicBool::new(false);
    let result = runtime.block_on(async {
        blocking_exchange(
            &mut process_fixture("hang"),
            Vec::new(),
            std::time::Instant::now() + Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
            permit,
            |child, request| {
                assert!(child.child.id() > 0);
                spawned.store(true, Ordering::SeqCst);
                child.start_pipes(request)
            },
        )
    });
    // The setup closure follows an actual std::Command::spawn of this test
    // binary, not a fake spawner/registration proxy. It must never be reached.
    assert!(!spawned.load(Ordering::SeqCst));
    assert_eq!(result, Err(HelperError::Helper));
    assert_eq!(slots.available_permits(), 1);
}

#[test]
fn setup_error_after_real_std_spawn_retains_capacity_through_actual_reap() {
    let _serial = process_test_lock();
    let runtime = process_runtime(true);
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let (reaped, exit) = oneshot::channel();
    let result = runtime.block_on(async {
        blocking_exchange(
            &mut process_fixture("hang"),
            Vec::new(),
            std::time::Instant::now() + Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
            permit,
            |child, _| {
                assert!(child.child.id() > 0);
                assert!(child.child.try_wait().unwrap().is_none());
                assert_eq!(slots.available_permits(), 0);
                child.exit_notice = Some(reaped);
                // Inject post-spawn pipe setup failure with an actual owned PID.
                Err(HelperError::Helper)
            },
        )
    });
    assert_eq!(result, Err(HelperError::Helper));
    assert!(exit.blocking_recv().is_ok());
    assert_eq!(slots.available_permits(), 1);
}

#[test]
fn setup_panic_after_real_std_spawn_still_reaps_the_owned_child() {
    let _serial = process_test_lock();
    let runtime = process_runtime(true);
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let (reaped, exit) = oneshot::channel();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            blocking_exchange(
                &mut process_fixture("hang"),
                Vec::new(),
                std::time::Instant::now() + Duration::from_secs(2),
                Arc::new(AtomicBool::new(false)),
                permit,
                |child, _| {
                    assert!(child.child.id() > 0);
                    child.exit_notice = Some(reaped);
                    panic!("injected post-spawn pipe setup panic");
                },
            )
        })
    }));
    assert!(result.is_err());
    assert!(exit.blocking_recv().is_ok());
    assert_eq!(slots.available_permits(), 1);
}

#[test]
fn dropping_waiter_kills_and_reaps_real_child_before_returning_capacity() {
    let _serial = process_test_lock();
    process_runtime(true).block_on(async {
        let slots = Arc::new(Semaphore::new(1));
        let permit = slots.clone().try_acquire_owned().unwrap();
        let (mut publication, waiter) = oneshot::channel::<Result<Vec<u8>, HelperError>>();
        let (started, running) = oneshot::channel();
        let (reaped, exit) = oneshot::channel();
        let owner = tokio::spawn(async move {
            run_owned_exchange(
                process_fixture("hang"),
                vec![0; MAC_HELPER_WIRE_LIMIT],
                Instant::now() + Duration::from_secs(5),
                &mut publication,
                permit,
                move |child, request| {
                    child.exit_notice = Some(reaped);
                    let _ = started.send(child.child.id());
                    child.start_pipes(request)
                },
            )
            .await
        });
        assert!(running.await.unwrap() > 0);
        assert_eq!(slots.available_permits(), 0);
        drop(waiter);
        assert_eq!(owner.await.unwrap(), Err(HelperError::Cancelled));
        tokio::time::timeout(CLEANUP_WAIT + Duration::from_secs(1), exit)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while slots.available_permits() != 1 {
                tokio::time::sleep(CHILD_POLL).await;
            }
        })
        .await
        .unwrap();
    });
}

#[test]
fn oversized_real_child_output_is_killed_and_reaped() {
    let _serial = process_test_lock();
    let runtime = process_runtime(true);
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let (reaped, exit) = oneshot::channel();
    let result = runtime.block_on(async {
        blocking_exchange(
            &mut process_fixture("oversized"),
            Vec::new(),
            std::time::Instant::now() + Duration::from_secs(3),
            Arc::new(AtomicBool::new(false)),
            permit,
            |child, request| {
                child.exit_notice = Some(reaped);
                child.start_pipes(request)
            },
        )
    });
    assert_eq!(result, Err(HelperError::Helper));
    assert!(exit.blocking_recv().is_ok());
    assert_eq!(slots.available_permits(), 1);
}

#[test]
fn deadline_kills_and_reaps_real_child_before_joining_blocked_pipes() {
    let _serial = process_test_lock();
    let runtime = process_runtime(true);
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let (reaped, exit) = oneshot::channel();
    let result = runtime.block_on(async {
        blocking_exchange(
            &mut process_fixture("hang"),
            vec![0; MAC_HELPER_WIRE_LIMIT],
            std::time::Instant::now() + Duration::from_millis(100),
            Arc::new(AtomicBool::new(false)),
            permit,
            |child, request| {
                child.exit_notice = Some(reaped);
                child.start_pipes(request)
            },
        )
    });
    assert_eq!(result, Err(HelperError::Deadline));
    assert!(exit.blocking_recv().is_ok());
    assert_eq!(slots.available_permits(), 1);
}

#[test]
fn std_spawn_failure_quarantines_the_pre_spawn_reservation() {
    let _serial = process_test_lock();
    let runtime = process_runtime(true);
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let missing = std::env::current_dir()
        .unwrap()
        .join("nonexistent-macos-helper-for-spawn-failure");
    let result =
        runtime.block_on(async { OwnedHelperChild::spawn(&mut Command::new(missing), permit) });
    assert!(matches!(result, Err(HelperError::CleanupPending)));
    assert_eq!(slots.available_permits(), 0);
}

#[test]
fn unreaped_pipe_worker_quarantines_even_after_actual_child_exit() {
    let _serial = process_test_lock();
    let runtime = process_runtime(true);
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let (release, stalled) = std::sync::mpsc::channel();
    let (finished, joined) = std::sync::mpsc::channel();
    runtime.block_on(async {
        let mut child = OwnedHelperChild::spawn(&mut process_fixture("hang"), permit).unwrap();
        child
            .add_pipe(false, move || {
                let _ = stalled.recv_timeout(CLEANUP_WAIT * 3);
                let _ = finished.send(());
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(child.cleanup(), Err(HelperError::CleanupPending));
        assert!(child.exit.is_some());
        drop(child);
    });
    assert_eq!(slots.available_permits(), 0);
    release.send(()).unwrap();
    joined.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(slots.available_permits(), 0);
}

#[test]
fn helper_metadata_is_inert_redacted_and_requires_a_finite_deadline() {
    let executable = std::env::current_dir().unwrap().join("not-executed-helper");
    let helper =
        MacKeychainHelper::new(executable.clone(), [4; 32], Duration::from_secs(2)).unwrap();
    assert!(!format!("{helper:?}").contains("not-executed"));
    assert!(MacKeychainHelper::new(executable.clone(), [4; 32], Duration::ZERO).is_err());
    assert!(
        MacKeychainHelper::new(executable, [4; 32], MAX_DEADLINE + Duration::from_secs(1)).is_err()
    );
    assert!(MacKeychainHelper::new("relative".into(), [4; 32], Duration::from_secs(1)).is_err());
}

#[test]
fn unexpected_worker_drop_cannot_release_an_unreaped_child_slot() {
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    drop(ChildPermit(Some(permit)));
    assert_eq!(slots.available_permits(), 0);
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn non_macos_signing_is_unavailable_without_executing_a_helper() {
    let executable = std::env::current_dir()
        .unwrap()
        .join("never-executed-helper");
    let helper = MacKeychainHelper::new(executable, [4; 32], Duration::from_secs(1)).unwrap();
    let signer = MacStoreSigner::new(selector(), helper);
    assert!(matches!(
        signer.sign_ps256(b"header.payload").await,
        Err(HeadlessError::Unavailable)
    ));
}
