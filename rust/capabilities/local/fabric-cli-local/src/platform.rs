// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::io::Write;
use std::process::Command;

use crate::LocalError;

/// Explicit browser launch boundary; tests never invoke the native implementation.
pub trait BrowserLauncher {
    /// Launch this fully built URL without evaluating a shell command string.
    fn open_new(&mut self, url: &str) -> Result<(), LocalError>;
}

/// Native platform browser adapter; constructing it does not open a browser.
///
/// Calling `open_new` is a user-visible action and requires the CLI host's
/// explicit user invocation. Tests use an injected capture instead.
pub struct SystemBrowserLauncher;

impl BrowserLauncher for SystemBrowserLauncher {
    fn open_new(&mut self, url: &str) -> Result<(), LocalError> {
        if !url.starts_with("https://app.fabric.microsoft.com/groups/") {
            return Err(LocalError::command(
                "InvalidInput",
                "Only the Fabric browser origin is supported",
            ));
        }
        #[cfg(target_os = "linux")]
        let mut command = Command::new("xdg-open");
        #[cfg(target_os = "macos")]
        let mut command = Command::new("open");
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("rundll32.exe");
            command.arg("url.dll,FileProtocolHandler");
            command
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        return Err(LocalError::command(
            "Unsupported",
            "No native browser adapter for this platform",
        ));
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let status = command.arg(url).status()?;
            if !status.success() {
                return Err(LocalError::command(
                    "BrowserLaunchFailed",
                    "The native browser launcher failed",
                ));
            }
            Ok(())
        }
    }
}

/// Explicit terminal clearing boundary, separate from output/error serialization.
pub trait TerminalControl {
    /// Clear the caller-selected terminal and report write failures.
    fn clear(&mut self) -> Result<(), LocalError>;
}

/// ANSI terminal adapter over a caller-owned writer, never process-global stdout.
pub struct AnsiTerminal<W: Write> {
    writer: W,
}

impl<W: Write> AnsiTerminal<W> {
    /// Bind a terminal writer; construction has no effect.
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Recover the caller's writer, useful for offline byte capture.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<W: Write> TerminalControl for AnsiTerminal<W> {
    fn clear(&mut self) -> Result<(), LocalError> {
        self.writer.write_all(b"\x1b[H\x1b[2J\x1b[3J")?;
        Ok(())
    }
}
