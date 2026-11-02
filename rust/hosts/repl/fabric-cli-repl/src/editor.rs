// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional Reedline adapter for real stdin/stdout terminals only.
//!
//! Construction checks actual terminal handles before constructing Reedline.
//! History is bounded and in memory; no path, database, external editor,
//! clipboard, completer vocabulary, or environment fallback is configured.

use std::{
    borrow::Cow,
    error::Error,
    fmt,
    io::{self, IsTerminal},
};

use reedline::{
    FileBackedHistory, History, HistoryItem, HistoryItemId, HistorySessionId, Prompt,
    PromptEditMode, PromptHistorySearch, Reedline, ReedlineError, ReedlineErrorVariants,
    SearchQuery, Signal,
};

use crate::{InputEvent, Options, OptionsError, PromptError, ShellInput, safe_prompt};

/// Explicit adapter failure, distinct from [`InputEvent::EndOfInput`].
#[derive(Debug)]
pub enum EditorError {
    /// Either actual stdin or actual stdout is redirected, not a terminal.
    UnsupportedTerminal,
    /// Caller-provided resource limits are invalid.
    Options(OptionsError),
    /// Prompt data exceeded the configured safe display limit.
    Prompt(PromptError),
    /// The terminal input operation failed.
    Read(io::Error),
    /// A submitted line exceeded the input byte limit and was not dispatched.
    InputTooLong,
    /// In-memory history refused an operation.
    History(ReedlineError),
    /// Reedline returned a signal this adapter has not explicitly enabled.
    UnsupportedSignal,
}

impl fmt::Display for EditorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTerminal => {
                f.write_str("Interactive input requires terminal stdin and stdout.")
            }
            Self::Options(error) => fmt::Display::fmt(error, f),
            Self::Prompt(error) => fmt::Display::fmt(error, f),
            Self::Read(_) => f.write_str("Interactive terminal input failed."),
            Self::InputTooLong => f.write_str("Interactive input exceeds the byte limit."),
            Self::History(_) => f.write_str("Interactive in-memory history failed."),
            Self::UnsupportedSignal => f.write_str("Unsupported interactive editor signal."),
        }
    }
}

impl Error for EditorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Options(error) => Some(error),
            Self::Prompt(error) => Some(error),
            Self::Read(error) => Some(error),
            Self::History(error) => Some(error),
            _ => None,
        }
    }
}

/// Explicitly constructed, blocking terminal input adapter with bounded history.
///
/// Submitted input and retained history are byte-bounded. Reedline does not
/// expose a hard limit on its live editing/undo buffer; this adapter therefore
/// must not be used as an untrusted streaming input reader. Use normal command
/// or batch input for redirected data, not a pseudo-interactive fallback.
pub struct ReedlineEditor {
    engine: Reedline,
    options: Options,
}

impl ReedlineEditor {
    /// Checks real terminal handles, then creates the optional editor.
    ///
    /// This does not read a line or enable raw mode. No history file is opened.
    /// Reedline 0.52.1 uses `FileBackedHistory::new(capacity)` for memory-only
    /// history; it has no `new(capacity, None)` overload.
    pub fn new(options: Options) -> Result<Self, EditorError> {
        ensure_terminal()?;
        options.validate().map_err(EditorError::Options)?;
        let history = BoundedHistory::new(options).map_err(EditorError::History)?;
        let engine = Reedline::create()
            .with_history(Box::new(history))
            .with_ansi_colors(false)
            .disable_hints();
        Ok(Self { engine, options })
    }
}

fn ensure_terminal() -> Result<(), EditorError> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        Ok(())
    } else {
        Err(EditorError::UnsupportedTerminal)
    }
}

impl ShellInput for ReedlineEditor {
    type Error = EditorError;

    fn read_input(&mut self, prompt: &str) -> Result<InputEvent, Self::Error> {
        ensure_terminal()?;
        let prompt = DisplayPrompt(
            safe_prompt(prompt, self.options.max_prompt_bytes).map_err(EditorError::Prompt)?,
        );
        let signal = self.engine.read_line(&prompt).map_err(EditorError::Read)?;
        let history_error = self.engine.take_history_save_error();
        // Reedline can accept a line even when History::save fails. Check the
        // original size first so an oversized line remains a typed input failure.
        let event = input_event(signal, self.options.max_line_bytes)?;
        if let Some(error) = history_error {
            return Err(EditorError::History(error));
        }
        Ok(event)
    }
}

fn input_event(signal: Signal, max_bytes: usize) -> Result<InputEvent, EditorError> {
    match signal {
        Signal::Success(line) if line.len() > max_bytes => Err(EditorError::InputTooLong),
        Signal::Success(line) => Ok(InputEvent::Line(line)),
        Signal::CtrlC => Ok(InputEvent::Interrupt),
        Signal::CtrlD => Ok(InputEvent::EndOfInput),
        _ => Err(EditorError::UnsupportedSignal),
    }
}

struct DisplayPrompt(String);

impl Prompt for DisplayPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("history search: ")
    }
}

struct BoundedHistory {
    inner: FileBackedHistory,
    max_line_bytes: usize,
}

impl BoundedHistory {
    fn new(options: Options) -> reedline::Result<Self> {
        Ok(Self {
            inner: FileBackedHistory::new(options.history_capacity)?,
            max_line_bytes: options.max_line_bytes,
        })
    }
}

impl History for BoundedHistory {
    fn save(&mut self, item: HistoryItem) -> reedline::Result<HistoryItem> {
        if item.command_line.len() > self.max_line_bytes {
            return Err(ReedlineError(ReedlineErrorVariants::OtherHistoryError(
                "Input exceeds the shell history byte limit",
            )));
        }
        self.inner.save(item)
    }

    fn load(&self, id: HistoryItemId) -> reedline::Result<HistoryItem> {
        self.inner.load(id)
    }

    fn count(&self, query: SearchQuery) -> reedline::Result<i64> {
        self.inner.count(query)
    }

    fn search(&self, query: SearchQuery) -> reedline::Result<Vec<HistoryItem>> {
        self.inner.search(query)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> reedline::Result<()> {
        self.inner.update(id, updater)
    }

    fn clear(&mut self) -> reedline::Result<()> {
        self.inner.clear()
    }

    fn delete(&mut self, id: HistoryItemId) -> reedline::Result<()> {
        self.inner.delete(id)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.inner.sync()
    }

    fn session(&self) -> Option<HistorySessionId> {
        self.inner.session()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_have_distinct_control_flow_and_never_execute_host_payloads() {
        assert_eq!(
            input_event(Signal::Success("ls".into()), 2).unwrap(),
            InputEvent::Line("ls".into())
        );
        assert_eq!(
            input_event(Signal::CtrlC, 2).unwrap(),
            InputEvent::Interrupt
        );
        assert_eq!(
            input_event(Signal::CtrlD, 2).unwrap(),
            InputEvent::EndOfInput
        );
        assert!(matches!(
            input_event(Signal::Success("éé".into()), 3),
            Err(EditorError::InputTooLong)
        ));
        assert!(matches!(
            input_event(Signal::HostCommand("never execute".into()), 99),
            Err(EditorError::UnsupportedSignal)
        ));
        assert!(matches!(
            input_event(Signal::ExternalBreak("not EOF".into()), 99),
            Err(EditorError::UnsupportedSignal)
        ));
    }

    #[test]
    fn actual_history_backend_is_bounded_in_memory_without_terminal_access() {
        let mut history = BoundedHistory::new(Options {
            max_line_bytes: 4,
            history_capacity: 2,
            ..Options::default()
        })
        .unwrap();
        for line in ["one", "two", "last"] {
            history.save(HistoryItem::from_command_line(line)).unwrap();
        }
        assert_eq!(history.count_all().unwrap(), 2);
        assert_eq!(
            history.load(HistoryItemId::new(0)).unwrap().command_line,
            "two"
        );
        assert!(
            history
                .save(HistoryItem::from_command_line("large"))
                .is_err()
        );
        assert_eq!(history.count_all().unwrap(), 2);
        history.sync().unwrap();
        history.clear().unwrap();
        assert_eq!(history.count_all().unwrap(), 0);
    }

    #[test]
    fn zero_history_capacity_retains_nothing() {
        let mut history = BoundedHistory::new(Options {
            history_capacity: 0,
            ..Options::default()
        })
        .unwrap();
        history.save(HistoryItem::from_command_line("ls")).unwrap();
        assert_eq!(history.count_all().unwrap(), 0);
    }

    #[test]
    fn display_prompt_has_no_default_directory_clock_or_extra_indicator() {
        let prompt = DisplayPrompt("fab:/日本語<workspace>$ ".into());
        assert_eq!(prompt.render_prompt_left(), "fab:/日本語<workspace>$ ");
        assert_eq!(prompt.render_prompt_right(), "");
        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Default), "");
        assert_eq!(prompt.render_prompt_multiline_indicator(), "");
    }

    #[test]
    fn adapter_implements_the_public_input_contract_without_opening_a_terminal() {
        fn accepts_input<T: ShellInput<Error = EditorError>>() {}
        accepts_input::<ReedlineEditor>();
        let _: fn(Options) -> Result<ReedlineEditor, EditorError> = ReedlineEditor::new;
    }
}
