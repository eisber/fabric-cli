// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::SelectionFile;
use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use fabric_cli_command_contracts::CommandError;
use fabric_core::{Binding, Clock, HttpTransport, OperationContext};
use fabric_identity::{AzureCliSession, BoundCredential, ProcessExecutor};
use fabric_sdk_http::SystemClock;
use std::{
    io::IsTerminal,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

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
