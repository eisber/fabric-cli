// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Hermetic native SDK-process cancellation and contained-descendant evidence.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use fabric_core::{CancellationToken, Clock, CoreError, ErrorKind, OperationContext};
use fabric_identity::{AzureCliSession, ProcessExecutor};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DIRECTORY: &str = "FAB_OFFLINE_PROCESS_DIRECTORY";
const SCENARIO: &str = "FAB_OFFLINE_PROCESS_SCENARIO";
const READINESS: Duration = Duration::from_secs(5);
const DEADLINE: Duration = Duration::from_secs(8);
const CLEANUP: Duration = Duration::from_secs(5);

struct RealClock;

#[async_trait]
impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall_now(&self) -> SystemTime {
        SystemTime::now()
    }

    async fn wait_until(&self, deadline: Instant, _: &OperationContext) -> Result<(), CoreError> {
        tokio::time::sleep(deadline.saturating_duration_since(self.now())).await;
        Ok(())
    }

    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        context.remaining_at(self.now())?;
        tokio::time::sleep(delay).await;
        context.remaining_at(self.now())?;
        Ok(())
    }
}

fn directory() -> PathBuf {
    PathBuf::from(std::env::var_os(DIRECTORY).expect("private fixture directory"))
}

fn record_pid(role: &str) {
    let path = directory().join(format!("{role}.pid"));
    let temporary = path.with_extension("ready");
    fs::write(&temporary, std::process::id().to_string()).unwrap();
    fs::rename(temporary, path).unwrap();
}

fn pid(path: &Path, role: &str) -> Option<u32> {
    match fs::read_to_string(path.join(format!("{role}.pid"))) {
        Ok(value) => Some(value.parse().expect("fixture PID")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("Cannot read fixture PID: {error}"),
    }
}

fn ready(path: &Path) -> bool {
    pid(path, "az").is_some() && pid(path, "descendant").is_some()
}

fn running(id: u32) -> bool {
    #[cfg(unix)]
    {
        let output = Command::new("ps")
            .args(["-p", &id.to_string(), "-o", "stat="])
            .output()
            .expect("native ps");
        let state = String::from_utf8(output.stdout).unwrap();
        let state = state.trim();
        if state.is_empty() {
            assert_eq!(output.status.code(), Some(1), "unexpected ps failure");
            return false;
        }
        assert!(output.status.success(), "cannot inspect fixture process");
        !state.starts_with('Z')
    }
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {id}"), "/FO", "CSV", "/NH"])
            .output()
            .expect("native tasklist");
        assert!(output.status.success(), "cannot inspect fixture process");
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .any(|line| line.split(',').nth(1) == Some(format!("\"{id}\"").as_str()))
    }
}

fn stop(id: u32) {
    if !running(id) {
        return;
    }
    #[cfg(unix)]
    let status = Command::new("kill")
        .args(["-KILL", &id.to_string()])
        .status()
        .expect("native PID cleanup");
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/PID", &id.to_string(), "/T", "/F"])
        .status()
        .expect("native PID cleanup");
    assert!(status.success() || !running(id), "fixture cleanup failed");
}

struct FixtureDirectory(PathBuf);

impl FixtureDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fabric-identity-process-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        for role in ["descendant", "az"] {
            if let Some(id) = pid(&self.0, role) {
                stop(id);
            }
        }
        fs::remove_dir_all(&self.0).expect("remove exact private fixture directory");
    }
}

struct Driver(Child);

impl Drop for Driver {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().expect("stop exact fixture driver");
        }
        self.0.wait().expect("reap fixture driver");
    }
}

fn descendant() -> ! {
    record_pid("descendant");
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[allow(
    clippy::zombie_processes,
    reason = "Fixture models a blocked or exited parent; production group/job teardown owns the child"
)]
fn fake_az() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.first().map(String::as_str), Some("account"));
    assert_eq!(args.get(1).map(String::as_str), Some("get-access-token"));
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--fixture-descendant")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    record_pid("az");
    if std::env::var(SCENARIO).unwrap() == "pipe-holder" {
        let end = Instant::now() + READINESS;
        while pid(&directory(), "descendant").is_none() {
            assert!(Instant::now() < end, "descendant readiness timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(child); // The descendant retains inherited credential pipes after az exits.
        return ExitCode::SUCCESS;
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

async fn drive(scenario: &str) {
    let path = directory();
    let cancellation = CancellationToken::new();
    let context = OperationContext::new(
        Instant::now(),
        if scenario == "cancel" {
            Duration::from_secs(30)
        } else {
            DEADLINE
        },
        cancellation.clone(),
    )
    .unwrap();
    let session = AzureCliSession::new(Arc::new(ProcessExecutor), Arc::new(RealClock)).unwrap();
    let start = Instant::now();
    let acquisition = session.login(&context);
    let control = async {
        let end = Instant::now() + READINESS;
        while !ready(&path) {
            assert!(Instant::now() < end, "owned processes never became ready");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            running(pid(&path, "descendant").unwrap()),
            "descendant must actually be running"
        );
        if scenario != "pipe-holder" {
            assert!(
                running(pid(&path, "az").unwrap()),
                "fake az must actually be running"
            );
        }
        if scenario == "cancel" {
            cancellation.cancel();
        }
    };
    let (result, ()) = tokio::join!(acquisition, control);
    let error = result.expect_err("fake az must never produce identity/token success");
    assert_eq!(
        error.kind,
        if scenario == "cancel" {
            ErrorKind::Cancelled
        } else {
            ErrorKind::Timeout
        }
    );
    assert!(
        start.elapsed()
            < if scenario == "cancel" {
                READINESS + Duration::from_secs(2)
            } else {
                DEADLINE + Duration::from_secs(2)
            },
        "credential acquisition did not return promptly"
    );
    let ids = [pid(&path, "az").unwrap(), pid(&path, "descendant").unwrap()];
    let end = Instant::now() + CLEANUP;
    while ids.iter().any(|id| running(*id)) {
        assert!(
            Instant::now() < end,
            "owned credential process survived cleanup grace"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn scenario(scenario: &str) {
    let directory = FixtureDirectory::new();
    let bin = directory.0.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(directory.0.join("config")).unwrap();
    fs::copy(
        std::env::current_exe().unwrap(),
        bin.join(if cfg!(windows) { "az.exe" } else { "az" }),
    )
    .unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut driver = Driver(
        Command::new(std::env::current_exe().unwrap())
            .args(["--fixture-driver", scenario])
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("AZURE_CONFIG_DIR", directory.0.join("config"))
            .env(DIRECTORY, &directory.0)
            .env(SCENARIO, scenario)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let end = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(status) = driver.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "credential process scenario {scenario} failed"
            );
            break;
        }
        assert!(Instant::now() < end, "credential process driver watchdog");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--fixture-descendant") => descendant(),
        Some("--fixture-driver") => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(drive(args.get(2).expect("fixture scenario")));
            ExitCode::SUCCESS
        }
        _ if std::env::current_exe()
            .unwrap()
            .file_stem()
            .is_some_and(|name| name == "az") =>
        {
            fake_az()
        }
        _ => {
            for name in ["cancel", "timeout", "pipe-holder"] {
                scenario(name);
            }
            ExitCode::SUCCESS
        }
    }
}
