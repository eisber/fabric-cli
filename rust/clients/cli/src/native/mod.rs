// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod selection;

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use fabric_cli_command_contracts::CommandError;
use fabric_core::{Binding, CancellationToken, Clock, HttpTransport, OperationContext};
use fabric_identity::{AzureCliSession, BoundCredential, ProcessExecutor};
use fabric_sdk_http::SystemClock;
pub use selection::SelectionFile;
use std::{
    io::{BufRead, IsTerminal, Read},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::oneshot;

use fabric_cli_command_contracts::Host;

/// Production composition with lazy configuration discovery.
#[derive(Default)]
pub struct NativeHost {
    selection_path: Option<PathBuf>,
    diagnostics: Arc<Mutex<Vec<&'static str>>>,
}

impl NativeHost {
    /// Inject an explicit Rust selection path, without opening it.
    pub fn with_selection_path(path: PathBuf) -> Self {
        Self {
            selection_path: Some(path),
            ..Default::default()
        }
    }

    fn file(&self) -> Result<SelectionFile, CommandError> {
        self.selection_path
            .clone()
            .map(SelectionFile::new)
            .map(Ok)
            .unwrap_or_else(SelectionFile::discover)
    }

    fn session(&self) -> Result<Arc<AzureCliSession>, CommandError> {
        Ok(Arc::new(AzureCliSession::new(
            Arc::new(ProcessExecutor),
            self.clock(),
        )?))
    }
}

struct NativeClock {
    diagnostics: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        SystemClock.now()
    }
    fn wall_now(&self) -> SystemTime {
        SystemClock.wall_now()
    }
    fn diagnostic(&self, message: &'static str) {
        self.diagnostics
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(message);
    }
    async fn wait_until(
        &self,
        deadline: Instant,
        context: &OperationContext,
    ) -> Result<(), fabric_core::CoreError> {
        SystemClock.wait_until(deadline, context).await
    }
    async fn sleep(
        &self,
        delay: Duration,
        context: &OperationContext,
    ) -> Result<(), fabric_core::CoreError> {
        SystemClock.sleep(delay, context).await
    }
}

#[async_trait]
impl Host for NativeHost {
    fn environment(&self, name: &str) -> Option<std::ffi::OsString> {
        std::env::var_os(name)
    }

    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(NativeClock {
            diagnostics: self.diagnostics.clone(),
        })
    }
    fn selection(&self) -> Result<Option<Binding>, CommandError> {
        self.file()?.read()
    }
    fn save_selection(&self, binding: &Binding) -> Result<(), CommandError> {
        self.file()?.write(binding)
    }
    fn clear_selection(&self) -> Result<(), CommandError> {
        self.file()?.clear()
    }
    async fn login(&self, context: &OperationContext) -> Result<Binding, CommandError> {
        Ok(self.session()?.login(context).await?)
    }
    fn credential(&self, binding: Binding) -> Result<Arc<dyn TokenCredential>, CommandError> {
        Ok(Arc::new(BoundCredential::new(self.session()?, binding)?))
    }
    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError> {
        Ok(Arc::new(fabric_sdk_transport::NativeHttpClient::new()?))
    }
    async fn confirm(&self, cancellation: CancellationToken) -> Result<bool, CommandError> {
        if !std::io::stdin().is_terminal() {
            return Err(CommandError::new(
                "InvalidInput",
                "Interactive confirmation requires a terminal; use --force",
            ));
        }
        let (sender, receiver) = oneshot::channel();
        std::thread::Builder::new()
            .spawn(move || {
                let stdin = std::io::stdin();
                let mut reader = std::io::BufReader::new(stdin.lock().take(1026));
                let mut line = Vec::new();
                let result = reader.read_until(b'\n', &mut line);
                let answer = match result {
                    Ok(_) => confirmation_answer(&line),
                    Err(_) => Err(CommandError::new(
                        "InvalidInput",
                        "Cannot read confirmation",
                    )),
                };
                let _ = sender.send(answer);
            })
            .map_err(|_| CommandError::new("InvalidInput", "Cannot start confirmation reader"))?;
        tokio::select! {
            _ = cancellation.cancelled() => Err(CommandError::new("Cancelled", "Confirmation cancelled")),
            result = receiver => result.map_err(|_| CommandError::new("InvalidInput", "Cannot read confirmation"))?,
        }
    }
    fn take_diagnostics(&self) -> Vec<&'static str> {
        std::mem::take(
            &mut *self
                .diagnostics
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    fn progress_color(&self) -> bool {
        self.progress_terminal() && std::env::var_os("NO_COLOR").is_none()
    }
    fn progress_terminal(&self) -> bool {
        std::io::stderr().is_terminal()
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
    }
}

fn confirmation_answer(line: &[u8]) -> Result<bool, CommandError> {
    if line.is_empty() {
        return Err(CommandError::new(
            "Cancelled",
            "Confirmation ended without input",
        ));
    }
    if line.len() > 1025
        || !line.ends_with(b"\n")
        || line
            .iter()
            .any(|byte| byte.is_ascii_control() && !matches!(*byte, b'\r' | b'\n'))
    {
        return Err(CommandError::new(
            "InvalidInput",
            "Invalid or oversized confirmation",
        ));
    }
    let text = std::str::from_utf8(line)
        .map_err(|_| CommandError::new("InvalidInput", "Invalid confirmation"))?;
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err(CommandError::new("InvalidInput", "Invalid confirmation")),
    }
}

#[cfg(test)]
mod tests {
    use super::confirmation_answer;

    #[test]
    fn confirmation_fails_closed() {
        for line in [b"\n".as_slice(), b"y\n", b"YES\r\n"] {
            assert!(confirmation_answer(line).unwrap());
        }
        for line in [b"n\n".as_slice(), b"NO\n"] {
            assert!(!confirmation_answer(line).unwrap());
        }
        for line in [b"".as_slice(), b"yes", b"maybe\n", b"\xff\n", b"\t\n"] {
            assert!(confirmation_answer(line).is_err());
        }
        assert!(confirmation_answer(&[b'x'; 1026]).is_err());
    }
}
