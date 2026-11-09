// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline executable protocol failures; never sends a valid store request.

#![cfg(feature = "interactive")]
#![deny(unnameable_test_items)]

use fabric_identity::interactive::macos::PROTECTED_WIRE_LIMIT;
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[cfg(not(target_os = "macos"))]
#[test]
fn public_macos_configuration_typechecks_but_access_is_explicitly_unsupported() {
    use fabric_identity::interactive::{
        InteractiveError, OsSessionStore, macos::MacProtectedStoreHelper,
    };
    use std::{path::PathBuf, time::Duration};

    let executable = if cfg!(windows) {
        PathBuf::from(r"C:\not-executed-protected-helper.exe")
    } else {
        PathBuf::from("/not-executed-protected-helper")
    };
    assert!(matches!(
        MacProtectedStoreHelper::new(executable, [4; 32], Duration::from_secs(1)),
        Err(InteractiveError::Unsupported)
    ));
    let _attach: fn(
        OsSessionStore,
        MacProtectedStoreHelper,
    ) -> Result<OsSessionStore, InteractiveError> = OsSessionStore::with_macos_helper;
}

fn helper(arguments: &[&str], request: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fabric-protected-store-helper"))
        .env_clear()
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let request = request.to_vec();
    let writer = std::thread::spawn(move || {
        // An argv failure is allowed to close its unused stdin immediately.
        let _ = input.write_all(&request);
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    output
}

#[test]
fn help_version_and_argv_failures_do_not_require_helper_trust_or_keychain_access() {
    for argument in ["--help", "--version"] {
        let output = helper(&[argument], b"");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stdout).starts_with("fabric-protected-store-helper")
        );
    }
    let output = helper(&["untrusted-argv-canary"], b"");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["code"], "invalid_input");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("canary"));
}

#[test]
fn malformed_other_family_duplicate_and_oversized_requests_fail_without_store_access() {
    for bytes in [
        b"".to_vec(),
        br#"{"schema":1,"selector":{},"signingInput":"AQ"}"#.to_vec(),
        br#"{"operation":"read","schema":"fabric-protected-store-v1","locator":"short"}"#.to_vec(),
        br#"{"operation":"read","operation":"delete","schema":"fabric-protected-store-v1","locator":"short"}"#.to_vec(),
        vec![b' '; PROTECTED_WIRE_LIMIT + 1],
    ] {
        let output = helper(&[], &bytes);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "error");
        assert_eq!(value["schema"], "fabric-protected-store-v1");
        assert_eq!(value["code"], "invalid_input");
    }
}
