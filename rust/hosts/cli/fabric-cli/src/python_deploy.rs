// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Deployment-only process boundary to the installed, authoritative Python CLI.

use fabric_cli_transfer::TransferCommand;
use std::{
    ffi::OsString,
    future::Future,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

const ENTRY: &str = r#"
import os
import signal

# Keep terminal input in the foreground group, but let Rust own the signals it
# handles. Otherwise Python could exit before the helper discovers descendants.
if os.name == "posix":
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, signal.SIG_IGN)

import importlib
import importlib.metadata
import json
import sys

def unavailable(message):
    print(json.dumps({"error": {"code": "NotConfigured", "message": message}}), file=sys.stderr)
    raise SystemExit(1)

if not (3, 10) <= sys.version_info[:2] < (3, 15):
    unavailable("Deployment requires Python 3.10 through 3.14. Select its executable with FAB_RUST_DEPLOY_PYTHON.")
try:
    importlib.metadata.distribution("ms-fabric-cli")
    importlib.metadata.distribution("fabric-cicd")
    import psutil
    backend = importlib.import_module("fabric_cicd")
    cli = importlib.import_module("fabric_cli.main")
    importlib.import_module("fabric_cli.commands.fs.fab_fs_deploy")
    if not callable(getattr(backend, "deploy_with_config", None)):
        raise ImportError()
except (ImportError, importlib.metadata.PackageNotFoundError):
    unavailable("Deployment requires ms-fabric-cli and its fabric-cicd dependencies in the selected Python environment. Install the supported ms-fabric-cli package beforehand; FAB_RUST_DEPLOY_PYTHON selects that environment's Python executable.")

sys.argv[0] = "fab"
try:
    cli.main()
finally:
    # The CLI owns its subprocesses; do not leave deployment children behind.
    children = psutil.Process().children(recursive=True)
    for child in children:
        try:
            child.terminate()
        except psutil.NoSuchProcess:
            pass
    _, alive = psutil.wait_procs(children, timeout=0.5)
    for child in alive:
        try:
            child.kill()
        except psutil.NoSuchProcess:
            pass
    _, alive = psutil.wait_procs(alive, timeout=0.5)
    if alive:
        unavailable("Deployment process cleanup could not be verified. Child or remote work may still complete; inspect it before retrying.")
"#;

// psutil is an existing required dependency of ms-fabric-cli. This separate,
// deployment-free helper also works when a future is dropped during execution.
const CLEANUP: &str = r#"
import sys
import psutil

try:
    root = psutil.Process(int(sys.argv[1]))
    processes = root.children(recursive=True) + [root]
except psutil.NoSuchProcess:
    raise SystemExit(0)
for process in processes:
    try:
        process.terminate()
    except psutil.NoSuchProcess:
        pass
_, alive = psutil.wait_procs(processes, timeout=0.5)
for process in alive:
    try:
        process.kill()
    except psutil.NoSuchProcess:
        pass
_, alive = psutil.wait_procs(alive, timeout=0.5)
raise SystemExit(1 if alive else 0)
"#;

/// Sanitized process-boundary errors; never retain executable paths or arguments.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum BridgeError {
    /// An executable selector must name one executable, not a command line.
    #[error("FAB_RUST_DEPLOY_PYTHON must name a Python executable, not an empty value.")]
    InvalidSelector,
    /// No configured interpreter could be started.
    #[error(
        "Cannot start deployment Python. Install Python 3.10 through 3.14 and ms-fabric-cli beforehand, or set FAB_RUST_DEPLOY_PYTHON to that environment's Python executable."
    )]
    Start,
    /// The OS failed to report the child's terminal status.
    #[error(
        "Waiting for Python deployment failed. Deployment effects may have occurred; do not retry without checking the target."
    )]
    Wait,
    /// Cancellation is not rollback or proof of a failed remote operation.
    #[error(
        "Python deployment cancelled and its child process stopped. Deployment effects may have occurred; cancellation is not rollback."
    )]
    Cancelled,
    /// Cleanup did not establish that the entire deployment process tree stopped.
    #[error(
        "Python deployment interrupted, but process-tree cleanup could not be verified. Child or remote work may still complete; inspect it before retrying."
    )]
    Cleanup,
    /// A signal handler could not be installed safely.
    #[error("Cannot initialize deployment cancellation handling; deployment was not started.")]
    Signal,
}

impl BridgeError {
    /// Repository-standard error code for executable diagnostics.
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSelector => "invalid_configuration",
            Self::Start => "NotConfigured",
            Self::Cancelled => "OperationCancelled",
            Self::Wait | Self::Cleanup | Self::Signal => "DeploymentFailed",
        }
    }

    /// Cancellation follows the Python command's exit-2 convention.
    pub(crate) const fn exit(&self) -> u8 {
        match self {
            Self::InvalidSelector | Self::Cancelled => 2,
            _ => 1,
        }
    }
}

/// Choose once; a configured interpreter is never retried with another provider.
pub(crate) fn interpreter(value: Option<OsString>) -> Result<OsString, BridgeError> {
    match value {
        Some(value) if value.is_empty() => Err(BridgeError::InvalidSelector),
        Some(value) => Ok(value),
        None => Ok(if cfg!(windows) { "python" } else { "python3" }.into()),
    }
}

/// Reconstruct semantic argv without joining, interpreting, or re-quoting values.
pub(crate) fn arguments(command: &TransferCommand) -> Vec<OsString> {
    let mut args = vec!["deploy".into()];
    if let Some(config) = &command.config {
        args.extend([OsString::from("--config"), config.into()]);
    }
    args.extend([OsString::from("--target_env"), (&command.target_env).into()]);
    if let Some(params) = &command.params {
        args.push("--params".into());
        args.extend(params.iter().map(OsString::from));
    }
    if command.force {
        args.push("--force".into());
    }
    if command.bulk_publish {
        args.push("--bulk_publish".into());
    }
    if let Some(format) = &command.output_format {
        args.extend([OsString::from("--output_format"), format.into()]);
    }
    args
}

fn process_command(interpreter: &OsString, script: &str, arguments: &[OsString]) -> Command {
    let mut command = Command::new(interpreter);
    command
        .args(["-I", "-B", "-u", "-c", script])
        .args(arguments);
    // Unix must inherit the foreground group: a background child reading the
    // inherited controlling terminal would stop on SIGTTIN. ENTRY leaves
    // handled-signal cancellation to Rust without changing terminal ownership.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
    }
    command
}

struct DeploymentChild {
    child: Child,
    interpreter: OsString,
    finished: bool,
}

impl DeploymentChild {
    fn stop(&mut self) -> Result<(), BridgeError> {
        if self.finished {
            return Ok(());
        }
        let mut cleanup = Command::new(&self.interpreter)
            .args(["-I", "-B", "-c", CLEANUP])
            .arg(self.child.id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
        let deadline = Instant::now() + Duration::from_secs(3);
        let verified = cleanup.as_mut().is_some_and(|cleanup| {
            loop {
                // The sibling helper cannot reap our Unix child. Reap it while
                // the helper verifies PID disappearance after kill escalation.
                #[cfg(unix)]
                let _ = self.child.try_wait();
                match cleanup.try_wait() {
                    Ok(Some(status)) => break status.success(),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        let _ = cleanup.kill();
                        let _ = cleanup.wait();
                        break false;
                    }
                }
            }
        });
        // Always reap the direct child, including a failed dependency preflight.
        let _ = self.child.kill();
        self.child.wait().map_err(|_| BridgeError::Cleanup)?;
        self.finished = true;
        if verified {
            Ok(())
        } else {
            Err(BridgeError::Cleanup)
        }
    }
}

impl Drop for DeploymentChild {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.stop();
        }
    }
}

/// Run exactly one Python deployment with inherited streams and scoped cleanup.
///
/// The injected cancellation future is also used by offline process tests.
pub(crate) async fn execute(
    interpreter: OsString,
    arguments: &[OsString],
    cancelled: impl Future<Output = ()>,
) -> Result<ExitStatus, BridgeError> {
    execute_script(interpreter, ENTRY, arguments, cancelled).await
}

async fn execute_script(
    interpreter: OsString,
    script: &str,
    arguments: &[OsString],
    cancelled: impl Future<Output = ()>,
) -> Result<ExitStatus, BridgeError> {
    tokio::pin!(cancelled);
    tokio::select! {
        biased;
        () = &mut cancelled => return Err(BridgeError::Cancelled),
        () = std::future::ready(()) => {}
    }
    let child = process_command(&interpreter, script, arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|_| BridgeError::Start)?;
    let mut process = DeploymentChild {
        child,
        interpreter,
        finished: false,
    };
    loop {
        match process.child.try_wait() {
            Ok(Some(status)) => {
                process.finished = true;
                return Ok(status);
            }
            Ok(None) => {}
            Err(_) => {
                process.stop()?;
                return Err(BridgeError::Wait);
            }
        }
        tokio::select! {
            biased;
            () = &mut cancelled => {
                process.stop()?;
                return Err(BridgeError::Cancelled);
            }
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

/// Install cancellation handlers before starting any deployment process.
pub(crate) fn run(command: &TransferCommand) -> Result<ExitStatus, BridgeError> {
    let interpreter = interpreter(std::env::var_os("FAB_RUST_DEPLOY_PYTHON"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| BridgeError::Signal)?;
    runtime.block_on(async {
        let interrupt = tokio::signal::ctrl_c();
        tokio::pin!(interrupt);
        // Poll registration before starting the process.
        tokio::select! {
            biased;
            _ = &mut interrupt => return Err(BridgeError::Signal),
            () = std::future::ready(()) => {}
        }
        #[cfg(unix)]
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| BridgeError::Signal)?;
        #[cfg(windows)]
        let mut interrupt_break =
            tokio::signal::windows::ctrl_break().map_err(|_| BridgeError::Signal)?;
        #[cfg(windows)]
        let mut close = tokio::signal::windows::ctrl_close().map_err(|_| BridgeError::Signal)?;
        #[cfg(windows)]
        let mut shutdown =
            tokio::signal::windows::ctrl_shutdown().map_err(|_| BridgeError::Signal)?;
        let cancelled = async {
            #[cfg(unix)]
            tokio::select! {
                _ = interrupt => {}
                _ = terminate.recv() => {}
            }
            #[cfg(windows)]
            tokio::select! {
                _ = interrupt => {}
                _ = interrupt_break.recv() => {}
                _ = close.recv() => {}
                _ = shutdown.recv() => {}
            }
            #[cfg(not(any(unix, windows)))]
            let _ = interrupt.await;
        };
        execute(interpreter, &arguments(command), cancelled).await
    })
}

/// Preserve ordinary Python statuses, and make signal termination non-successful.
pub(crate) fn exit_code(status: ExitStatus) -> std::process::ExitCode {
    #[cfg(windows)]
    {
        let code = status.code().unwrap_or(1);
        match u8::try_from(code) {
            Ok(code) => std::process::ExitCode::from(code),
            // Stable ExitCode cannot represent Windows exception statuses.
            // The runtime and child have already been shut down and reaped.
            Err(_) => std::process::exit(code),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let code = status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
        std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
    }
    #[cfg(not(any(windows, unix)))]
    std::process::ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Files(PathBuf);

    impl Files {
        fn new() -> Self {
            let root = PathBuf::from("target").join(format!(
                "python-deploy-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn python() -> OsString {
        interpreter(std::env::var_os("FAB_RUST_DEPLOY_TEST_PYTHON")).unwrap()
    }

    fn fake_backend(body: &str) -> String {
        format!(
            r#"
import importlib.metadata
import sys
import types
importlib.metadata.distribution = lambda _: object()
backend = types.ModuleType("fabric_cicd")
backend.deploy_with_config = lambda: None
sys.modules["fabric_cicd"] = backend
cli = types.ModuleType("fabric_cli.main")
def main():
{body}
cli.main = main
sys.modules["fabric_cli.main"] = cli
sys.modules["fabric_cli.commands.fs.fab_fs_deploy"] = types.ModuleType("deployment_fixture")
{ENTRY}
"#
        )
    }

    #[test]
    fn authoritative_entry_receives_values_and_streams_and_exits_once() {
        let fabric_cli::parser::Invocation::Deploy(command) = fabric_cli::parser::parse_args([
            "fab-rs",
            "deploy",
            "--config",
            "config dir\\quoted \"東京\".yml",
            "-tenv",
            "dev space é",
            "--params",
            r#"[{"text":"spaces 'single' \"double\" 東京; & | $"}]"#,
            "--force",
            "--bulk_publish",
            "--output_format",
            "json",
        ])
        .unwrap() else {
            panic!("deployment command expected")
        };
        let args = arguments(&command);
        for exit in [0, 1, 2, 4, 17] {
            let body = format!(
                "    import json\n    print(json.dumps(sys.argv[1:], ensure_ascii=True))\n    sys.stdout.buffer.write(b'output\\x00bytes\\n')\n    sys.stderr.buffer.write(b'error\\x00bytes\\n')\n    raise SystemExit({exit})"
            );
            let output = process_command(&python(), &fake_backend(&body), &args)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(exit));
            let newline = output
                .stdout
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap();
            let received: Vec<String> = serde_json::from_slice(&output.stdout[..newline]).unwrap();
            assert_eq!(
                received,
                args.iter()
                    .map(|arg| arg.to_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            );
            assert_eq!(&output.stdout[newline + 1..], b"output\0bytes\n");
            assert_eq!(output.stderr, b"error\0bytes\n");
        }
    }

    #[test]
    fn missing_package_and_unsupported_runtime_are_actionable_and_secret_safe() {
        for prefix in [
            "import importlib.metadata\nimportlib.metadata.distribution = lambda _: (_ for _ in ()).throw(importlib.metadata.PackageNotFoundError('secret package path'))\n",
            "import sys\nsys.version_info = (3, 9)\n",
        ] {
            let output = process_command(
                &python(),
                &format!("{prefix}{ENTRY}"),
                &["deploy".into(), "--config".into(), "secret config".into()],
            )
            .output()
            .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "NotConfigured");
            let message = error["error"]["message"].as_str().unwrap();
            assert!(message.contains("FAB_RUST_DEPLOY_PYTHON"));
            assert!(!message.contains("secret"));
        }
        assert_eq!(
            interpreter(Some(OsString::new())),
            Err(BridgeError::InvalidSelector)
        );
        assert_eq!(
            interpreter(Some("path with spaces\\python.exe".into())).unwrap(),
            OsString::from("path with spaces\\python.exe")
        );
    }

    #[test]
    fn global_output_and_source_environment_alias_are_forwarded() {
        let fabric_cli::parser::Invocation::Deploy(command) = fabric_cli::parser::parse_args([
            "fab-rs",
            "--output_format",
            "json",
            "deploy",
            "--config=config path.yml",
            "-tenv=東京 environment",
        ])
        .unwrap() else {
            panic!("deployment command expected")
        };
        assert_eq!(
            arguments(&command),
            [
                "deploy",
                "--config",
                "config path.yml",
                "--target_env",
                "東京 environment",
                "--output_format",
                "json",
            ]
            .map(OsString::from)
        );
    }

    #[tokio::test]
    async fn start_failure_and_precancellation_never_execute_or_fallback() {
        let missing: OsString = "missing secret interpreter executable".into();
        let error = execute(missing.clone(), &[], std::future::pending())
            .await
            .unwrap_err();
        assert_eq!(error, BridgeError::Start);
        assert!(!error.to_string().contains("secret"));
        assert_eq!(
            execute(missing, &[], std::future::ready(())).await,
            Err(BridgeError::Cancelled)
        );
    }

    #[tokio::test]
    async fn a_failed_deployment_is_not_replayed() {
        let files = Files::new();
        let marker = files.0.join("execution-count");
        let script = "import pathlib,sys; p=pathlib.Path(sys.argv[1]); p.write_text(p.read_text()+'x' if p.exists() else 'x'); raise SystemExit(17)";
        let status = execute_script(
            python(),
            script,
            &[marker.clone().into_os_string()],
            std::future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(status.code(), Some(17));
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
    }

    async fn wait_ready(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < deadline, "fixture did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn cancellation_and_dropped_future_stop_and_reap_the_process_tree() {
        let files = Files::new();
        let script = r#"
import json, os, pathlib, subprocess, sys, time
child = subprocess.Popen([sys.executable, "-I", "-B", "-c", "import time; time.sleep(60)"])
pathlib.Path(sys.argv[1]).write_text(json.dumps([os.getpid(), child.pid]))
time.sleep(60)
"#;
        for dropped in [false, true] {
            let ready = files.0.join(format!("ready-{dropped}"));
            let arguments = [ready.clone().into_os_string()];
            if dropped {
                let execution =
                    execute_script(python(), script, &arguments, std::future::pending());
                tokio::pin!(execution);
                tokio::select! {
                    result = &mut execution => panic!("unexpected fixture exit: {result:?}"),
                    () = wait_ready(&ready) => {}
                }
            } else {
                assert_eq!(
                    execute_script(python(), script, &arguments, wait_ready(&ready)).await,
                    Err(BridgeError::Cancelled)
                );
            }
            let pids: Vec<u32> = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
            let output = process_command(
                &python(),
                "import psutil,sys; raise SystemExit(any(psutil.pid_exists(int(pid)) for pid in sys.argv[1:]))",
                &pids.iter().map(|pid| pid.to_string().into()).collect::<Vec<_>>(),
            )
            .output()
            .unwrap();
            assert!(output.status.success(), "deployment process tree survived");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signal_termination_is_never_success() {
        let status = execute_script(
            python(),
            "import os,signal; os.kill(os.getpid(), signal.SIGTERM)",
            &[],
            std::future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(exit_code(status), std::process::ExitCode::from(143));
    }
}
