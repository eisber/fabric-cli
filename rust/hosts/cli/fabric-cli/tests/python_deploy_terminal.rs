// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline Unix controlling-terminal coverage of the actual deployment adapter.

#![deny(unnameable_test_items)]

#[cfg(unix)]
#[test]
fn nonforced_deployment_preserves_terminal_confirmation_and_ctrl_c_cleanup() {
    let python =
        std::env::var_os("FAB_RUST_DEPLOY_TEST_PYTHON").unwrap_or_else(|| "python3".into());
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("python-deploy-pty-{}", std::process::id()));
    let output = std::process::Command::new(python)
        .args([
            "-I",
            "-B",
            "-u",
            "-c",
            include_str!("python_deploy_terminal.py"),
            env!("CARGO_BIN_EXE_fab-rs"),
        ])
        .arg(root)
        .output()
        .expect("Unix PTY coverage requires the test Python and its existing psutil dependency");
    assert!(
        output.status.success(),
        "PTY regression failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
