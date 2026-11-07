// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use crate::macos_helper::{
    CHILD_POLL, CLEANUP_WAIT, Cancellation, HelperError, OwnedHelperChild, blocking_exchange,
};
use std::{
    cell::RefCell,
    io::{Read, Write},
    process::Command,
    sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
    thread,
};
use tokio::sync::{Semaphore, oneshot};

fn locator() -> String {
    "a".repeat(64)
}

struct Fake {
    calls: RefCell<Vec<(String, Vec<u8>)>>,
    status: Result<(), i32>,
}

impl ProtectedBackend for Fake {
    fn read(&self, account: &str) -> Result<Vec<u8>, i32> {
        self.calls.borrow_mut().push((account.into(), Vec::new()));
        self.status.map(|()| b"synthetic-refresh-canary".to_vec())
    }
    fn upsert(&self, account: &str, bytes: &[u8]) -> Result<(), i32> {
        self.calls
            .borrow_mut()
            .push((account.into(), bytes.to_vec()));
        self.status
    }
    fn delete(&self, account: &str) -> Result<(), i32> {
        self.calls.borrow_mut().push((account.into(), Vec::new()));
        self.status
    }
}

#[test]
fn closed_protocol_preserves_exact_locator_and_binary_bytes_for_every_operation() {
    assert_eq!(PROTECTED_SERVICE, "Microsoft.Fabric.Cli.Rust.DeviceCode.v1");
    let account = locator();
    let bytes = b"\0\xffsynthetic-refresh-canary";
    for request in [
        ProtectedRequest::read(&account),
        ProtectedRequest::upsert(&account, bytes),
        ProtectedRequest::delete(&account),
    ] {
        let encoded = request.encode().unwrap();
        assert!(encoded.len() <= PROTECTED_WIRE_LIMIT);
        assert!(!String::from_utf8_lossy(&encoded).contains("synthetic-refresh-canary"));
        let decoded = ProtectedRequest::decode(&encoded).unwrap();
        let backend = Fake {
            calls: RefCell::new(Vec::new()),
            status: Ok(()),
        };
        let reply = decoded.execute(&backend).encode().unwrap();
        let result = ProtectedReply::verify(&reply, &request).unwrap();
        assert_eq!(backend.calls.borrow()[0].0, account);
        match request {
            ProtectedRequest::Read { .. } => assert_eq!(result, b"synthetic-refresh-canary"),
            ProtectedRequest::Upsert { .. } => {
                assert!(result.is_empty());
                assert_eq!(backend.calls.borrow()[0].1, bytes);
            }
            ProtectedRequest::Delete { .. } => assert!(result.is_empty()),
        }
    }
}

#[test]
fn invalid_or_certificate_family_requests_fail_before_backend_access() {
    let encoded = ProtectedRequest::upsert(&locator(), b"test")
        .encode()
        .unwrap();
    let valid: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    for (field, value) in [
        ("unexpected", serde_json::json!(true)),
        ("schema", serde_json::json!(1)),
        ("schema", serde_json::json!("certificate-sign-v1")),
        ("locator", serde_json::json!("ambient")),
        ("locator", serde_json::json!("A".repeat(64))),
        ("secret", serde_json::json!("")),
        ("secret", serde_json::json!("dGVzdA==")),
        (
            "secret",
            serde_json::json!(URL_SAFE_NO_PAD.encode(vec![0; SECRET_LIMIT + 1])),
        ),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        assert!(ProtectedRequest::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    let duplicate = format!(
        "{{\"locator\":\"{}\",{}",
        locator(),
        std::str::from_utf8(&encoded)
            .unwrap()
            .trim_start_matches('{')
    );
    for invalid in [
        duplicate.into_bytes(),
        [encoded.as_slice(), b" {}"].concat(),
        br#"{"schema":1,"selector":{},"signingInput":"AQ"}"#.to_vec(),
        vec![b' '; PROTECTED_WIRE_LIMIT + 1],
    ] {
        assert!(ProtectedRequest::decode(&invalid).is_err());
    }
    let backend = Fake {
        calls: RefCell::new(Vec::new()),
        status: Ok(()),
    };
    let invalid = ProtectedRequest::read("not-a-locator");
    let reply = invalid.execute(&backend).encode().unwrap();
    assert!(ProtectedReply::verify(&reply, &invalid).is_err());
    assert!(backend.calls.borrow().is_empty());
}

#[test]
fn only_actual_native_not_found_is_an_idempotent_delete() {
    for status in [
        Ok(()),
        Err(-25300),
        Err(-25308),
        Err(-25293),
        Err(-50),
        Err(-1),
    ] {
        let backend = Fake {
            calls: RefCell::new(Vec::new()),
            status,
        };
        for request in [
            ProtectedRequest::read(&locator()),
            ProtectedRequest::upsert(&locator(), b"test"),
            ProtectedRequest::delete(&locator()),
        ] {
            let reply = request.execute(&backend).encode().unwrap();
            let actual = ProtectedReply::verify(&reply, &request);
            let expected = status.is_ok()
                || (status == Err(-25300) && matches!(request, ProtectedRequest::Delete { .. }));
            assert_eq!(actual.is_ok(), expected);
            assert!(!format!("{actual:?}").contains("synthetic-refresh-canary"));
        }
    }
}

#[test]
fn replies_are_bounded_closed_canonical_and_operation_specific() {
    let read = ProtectedRequest::read(&locator());
    let delete = ProtectedRequest::delete(&locator());
    let done = ProtectedReply::Done {
        schema: SCHEMA.into(),
    }
    .encode()
    .unwrap();
    let data = ProtectedReply::Read {
        schema: SCHEMA.into(),
        secret: URL_SAFE_NO_PAD.encode(b"test"),
    }
    .encode()
    .unwrap();
    assert!(ProtectedReply::verify(&done, &read).is_err());
    assert!(ProtectedReply::verify(&data, &delete).is_err());
    for invalid in [
        br#"{"status":"read","schema":"fabric-protected-store-v1","secret":"dGVzdA=="}"#.as_slice(),
        br#"{"status":"read","schema":"fabric-protected-store-v1","secret":""}"#.as_slice(),
        br#"{"status":"done","schema":"fabric-protected-store-v1","secret":"canary"}"#.as_slice(),
        br#"{"status":"done","schema":1}"#.as_slice(),
        br#"{"status":"done","schema":"fabric-protected-store-v1","schema":"fabric-protected-store-v1"}"#.as_slice(),
        br#"{"status":"error","schema":"fabric-protected-store-v1","code":"raw-native-canary"}"#.as_slice(),
        br#"{"status":"done","schema":"fabric-protected-store-v1"} {}"#.as_slice(),
    ] {
        let error = ProtectedReply::verify(invalid, &delete).unwrap_err();
        assert!(!format!("{error} {error:?}").contains("canary"));
    }
    assert!(ProtectedReply::verify(&vec![b' '; PROTECTED_WIRE_LIMIT + 1], &read).is_err());
    let largest = ProtectedRequest::upsert(&locator(), &vec![0xff; SECRET_LIMIT]);
    assert!(largest.encode().unwrap().len() <= PROTECTED_WIRE_LIMIT);
    let oversized = ProtectedRequest::upsert(&locator(), &vec![0; SECRET_LIMIT + 1]);
    assert!(oversized.encode().is_err());
}

#[test]
fn configuration_is_inert_redacted_and_requires_finite_absolute_trust() {
    let executable = std::env::current_dir()
        .unwrap()
        .join("not-executed-protected-helper");
    #[cfg(target_os = "macos")]
    {
        let helper =
            MacProtectedStoreHelper::new(executable.clone(), [4; 32], Duration::from_secs(2))
                .unwrap();
        assert!(!format!("{helper:?}").contains("not-executed"));
    }
    for duration in [Duration::ZERO, Duration::from_secs(121)] {
        assert!(MacProtectedStoreHelper::new(executable.clone(), [4; 32], duration).is_err());
    }
    assert!(
        MacProtectedStoreHelper::new("relative".into(), [4; 32], Duration::from_secs(1)).is_err()
    );
    #[cfg(not(target_os = "macos"))]
    {
        assert!(matches!(
            MacProtectedStoreHelper::new(executable, [4; 32], Duration::from_secs(1)),
            Err(InteractiveError::Unsupported)
        ));
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_trusted_exchange_never_executes_an_explicit_helper() {
    use crate::macos_helper::{Helper, trusted_blocking_exchange};
    let path = std::env::current_dir()
        .unwrap()
        .join("never-executed-protected-helper");
    let helper = Helper::new(path, [4; 32], Duration::from_secs(1)).unwrap();
    assert_eq!(helper.sha256, [4; 32]);
    assert_eq!(helper.deadline, Duration::from_secs(1));
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    assert_eq!(
        trusted_blocking_exchange(
            &helper,
            fixture_request('3'),
            std::time::Instant::now() + helper.deadline,
            Arc::new(AtomicBool::new(false)),
            permit,
            PROTECTED_WIRE_LIMIT,
        ),
        Err(HelperError::UnsupportedPlatform)
    );
    assert_eq!(slots.available_permits(), 1);
}

fn process_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn runtime(io: bool) -> tokio::runtime::Runtime {
    let mut builder = tokio::runtime::Builder::new_current_thread();
    builder.enable_time();
    if io {
        builder.enable_io();
    }
    builder.build().unwrap()
}

fn fixture() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    let module = module_path!().split_once("::").unwrap().1;
    command.env_clear().args([
        "--exact",
        &format!("{module}::protected_process_fixture"),
        "--nocapture",
    ]);
    command
}

fn fixture_request(mode: char) -> Vec<u8> {
    let account = format!("{}{mode}", "0".repeat(63));
    ProtectedRequest::upsert(&account, b"synthetic-pipe-canary")
        .encode()
        .unwrap()
}

// Only an exact child invocation runs this body. It accesses no native store,
// network or credential. Mode and synthetic secret arrive only over stdin.
#[test]
fn protected_process_fixture() {
    let name = format!(
        "{}::protected_process_fixture",
        module_path!().split_once("::").unwrap().1
    );
    if !std::env::args().any(|arg| arg == name) {
        return;
    }
    assert!(!std::env::args().any(|arg| arg.contains("canary")));
    assert!(std::env::vars_os().next().is_none());
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take((PROTECTED_WIRE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    let request = ProtectedRequest::decode(&bytes).unwrap();
    let ProtectedRequest::Upsert {
        locator, secret, ..
    } = request
    else {
        panic!("wrong operation");
    };
    assert_eq!(decode_secret(&secret).unwrap(), b"synthetic-pipe-canary");
    match locator.as_bytes()[63] {
        b'1' => loop {
            thread::sleep(Duration::from_secs(60));
        },
        b'2' => std::io::stdout()
            .lock()
            .write_all(&vec![b'x'; PROTECTED_WIRE_LIMIT + 1])
            .unwrap(),
        b'3' => std::io::stdout()
            .lock()
            .write_all(
                &ProtectedReply::Done {
                    schema: SCHEMA.into(),
                }
                .encode()
                .unwrap(),
            )
            .unwrap(),
        b'4' => std::io::stderr()
            .lock()
            .write_all(b"synthetic-stderr-canary")
            .unwrap(),
        _ => panic!("wrong fixture mode"),
    }
    std::io::stdout().lock().flush().unwrap();
    std::io::stderr().lock().flush().unwrap();
    std::process::exit(0);
}

fn start_protected_pipes(
    child: &mut OwnedHelperChild,
    request: Vec<u8>,
) -> Result<(), HelperError> {
    // Same per-protocol bound used by trusted_blocking_exchange in production.
    child.set_wire_limit(PROTECTED_WIRE_LIMIT);
    child.start_pipes(request)
}

#[test]
fn real_fixture_receives_only_piped_secret_and_is_reaped_on_success_or_pipe_failure() {
    let _serial = process_lock();
    for mode in ['2', '3', '4'] {
        let slots = Arc::new(Semaphore::new(1));
        let permit = slots.clone().try_acquire_owned().unwrap();
        let (reaped, exit) = oneshot::channel();
        let outcome = runtime(true).block_on(async {
            blocking_exchange(
                &mut fixture(),
                fixture_request(mode),
                std::time::Instant::now() + Duration::from_secs(3),
                Arc::new(AtomicBool::new(false)),
                permit,
                move |child, request| {
                    child.exit_notice = Some(reaped);
                    start_protected_pipes(child, request)
                },
            )
        });
        if mode == '3' {
            // The test harness emits a prefix; the real helper has no harness.
            assert!(
                outcome.unwrap().ends_with(
                    &ProtectedReply::Done {
                        schema: SCHEMA.into(),
                    }
                    .encode()
                    .unwrap()
                )
            );
        } else {
            assert_eq!(outcome, Err(HelperError::Helper));
        }
        assert!(exit.blocking_recv().is_ok());
        assert_eq!(slots.available_permits(), 1);
    }
}

#[test]
fn real_protected_fixture_deadline_and_caller_drop_reap_before_capacity_returns() {
    let _serial = process_lock();
    for cancel in [false, true] {
        runtime(true).block_on(async {
            let slots = Arc::new(Semaphore::new(1));
            let permit = slots.clone().try_acquire_owned().unwrap();
            let cancellation = Cancellation::new();
            let control = cancellation.flag();
            let (started, running) = oneshot::channel();
            let (reaped, exit) = oneshot::channel();
            let worker = tokio::task::spawn_blocking(move || {
                blocking_exchange(
                    &mut fixture(),
                    fixture_request('1'),
                    std::time::Instant::now()
                        + Duration::from_millis(if cancel { 3000 } else { 100 }),
                    control,
                    permit,
                    move |child, request| {
                        child.exit_notice = Some(reaped);
                        let _ = started.send(child.child.id());
                        start_protected_pipes(child, request)
                    },
                )
            });
            assert!(running.await.unwrap() > 0);
            assert_eq!(slots.available_permits(), 0);
            if cancel {
                drop(cancellation);
            }
            assert_eq!(
                worker.await.unwrap(),
                Err(if cancel {
                    HelperError::Cancelled
                } else {
                    HelperError::Deadline
                })
            );
            tokio::time::timeout(CLEANUP_WAIT + Duration::from_secs(1), exit)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(slots.available_permits(), 1);
        });
    }
}

#[test]
fn real_protected_fixture_is_never_spawned_without_io_driver() {
    let _serial = process_lock();
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let outcome = runtime(false).block_on(async {
        blocking_exchange(
            &mut fixture(),
            fixture_request('1'),
            std::time::Instant::now() + Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
            permit,
            |_, _| panic!("spawn must not occur without I/O driver"),
        )
    });
    assert_eq!(outcome, Err(HelperError::Helper));
    assert_eq!(slots.available_permits(), 1);
}

#[test]
fn fixture_spawn_uncertainty_quarantines_protected_capacity() {
    let _serial = process_lock();
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let missing = std::env::current_dir()
        .unwrap()
        .join("not-installed-protected-helper");
    let outcome = runtime(true).block_on(async {
        blocking_exchange(
            &mut Command::new(missing),
            fixture_request('1'),
            std::time::Instant::now() + CHILD_POLL * 10,
            Arc::new(AtomicBool::new(false)),
            permit,
            start_protected_pipes,
        )
    });
    assert_eq!(outcome, Err(HelperError::CleanupPending));
    assert_eq!(slots.available_permits(), 0);
}
