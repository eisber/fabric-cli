// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Removable interactive shell orchestration, independent of application commands.
//!
//! The default feature set contains only bounded tokenization, prompt escaping,
//! and the explicitly invoked [`run`] loop. It has no terminal, SDK, identity,
//! runtime, logging, or persistence dependency. The optional `reedline-editor`
//! feature adds `editor`; callers still own command parsing and execution.
//!
//! Handled subparser/command failures should be rendered by the caller and return
//! [`CommandOutcome::Continue`]. An error from the host or input adapter is fatal
//! to this invocation and is never converted into successful shell completion.

#![deny(unnameable_test_items)]

use std::{error::Error, fmt};

#[cfg(feature = "reedline-editor")]
pub mod editor;

/// Finite UTF-8 byte, token, prompt, and in-memory history limits.
///
/// Byte limits apply to submitted lines, not Reedline's live editing/undo buffer.
/// The history payload is bounded by `history_capacity * max_line_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Maximum bytes in an input line, before trimming or tokenization.
    pub max_line_bytes: usize,
    /// Maximum number of tokens in a submitted command, including empty tokens.
    pub max_tokens: usize,
    /// Maximum bytes in both the original and escaped display prompt.
    pub max_prompt_bytes: usize,
    /// Maximum retained input lines; zero explicitly disables history retention.
    pub history_capacity: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_line_bytes: 64 * 1024,
            max_tokens: 1024,
            max_prompt_bytes: 4096,
            history_capacity: 128,
        }
    }
}

impl Options {
    /// Rejects unusable limits and a history payload whose byte budget overflows.
    pub fn validate(self) -> Result<(), OptionsError> {
        if self.max_line_bytes == 0 || self.max_tokens == 0 || self.max_prompt_bytes == 0 {
            return Err(OptionsError::ZeroLimit);
        }
        if self.history_capacity == usize::MAX
            || self
                .history_capacity
                .checked_mul(self.max_line_bytes)
                .is_none()
        {
            return Err(OptionsError::HistoryBudgetOverflow);
        }
        Ok(())
    }
}

/// Invalid caller-provided shell limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionsError {
    /// Input, token, and prompt limits must be nonzero.
    ZeroLimit,
    /// The history capacity or its total byte budget cannot be represented.
    HistoryBudgetOverflow,
}

impl fmt::Display for OptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ZeroLimit => "Shell input, token, and prompt limits must be nonzero.",
            Self::HistoryBudgetOverflow => "Shell history byte budget is too large.",
        })
    }
}

impl Error for OptionsError {}

/// An input event, distinct from an adapter read error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// A complete, owned command line.
    Line(String),
    /// Ctrl+C while entering a command; display a hint and keep reading.
    Interrupt,
    /// End of input; stop once without retrying the terminal.
    EndOfInput,
}

/// Explicit input boundary, implementable by offline fakes without editor features.
pub trait ShellInput {
    /// The adapter's distinct read failure type.
    type Error;

    /// Reads one event using the caller's current, already escaped display prompt.
    fn read_input(&mut self, prompt: &str) -> Result<InputEvent, Self::Error>;
}

/// Command control flow, separate from a command's application-specific exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Keep the shell alive, including after a handled parser/command failure.
    Continue,
    /// Stop at the command host's explicit request.
    Stop,
}

/// Presentation requests; application help and version remain owned by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellNotice {
    /// Initial shell greeting.
    Welcome,
    /// Display the application's interactive command help.
    Help,
    /// Display the application's version.
    Version,
    /// Explain that interactive commands do not need the executable prefix.
    PrefixHint,
    /// Explain how to leave after Ctrl+C.
    InterruptHint,
    /// Farewell for an exact, unquoted quit alias.
    Exit,
}

impl ShellNotice {
    /// Source text for fixed notices; help/version must use the current application.
    #[must_use]
    pub const fn message(self) -> Option<&'static str> {
        match self {
            Self::Welcome => Some("\nWelcome to the Fabric CLI ⚡\nType 'help' for help. \n"),
            Self::PrefixHint => Some(
                "In interactive mode, commands don't require the fab prefix. Use --help to view the list of supported commands.",
            ),
            Self::InterruptHint => Some("\nUse 'quit' or 'exit' to leave interactive mode."),
            Self::Exit => Some("Exiting interactive mode. Goodbye!"),
            Self::Help | Self::Version => None,
        }
    }
}

/// Caller-owned application bridge; this crate never depends on the application.
pub trait ShellHost {
    /// The caller's fatal host/presentation error type.
    type Error;

    /// Supplies a fresh complete display prompt before every input event.
    ///
    /// For source layout the caller supplies `fab:/<current-context>$ `.
    /// Do not include ANSI styling; this crate escapes terminal controls.
    fn prompt(&mut self) -> Result<String, Self::Error>;

    /// Parses and executes owned tokens through the application's normal host.
    ///
    /// Handle caught subparser errors/nonzero command statuses here and return
    /// [`CommandOutcome::Continue`]. Return `Err` to terminate this shell with a
    /// failure. The source catches subparser `SystemExit`, but not the root
    /// parser error for an unknown command; the caller owns this distinction.
    /// No shell expansion or operator execution is performed.
    fn execute(&mut self, tokens: Vec<String>) -> Result<CommandOutcome, Self::Error>;

    /// Renders a notice on caller-owned streams, including current help/version.
    fn notice(&mut self, notice: ShellNotice) -> Result<(), Self::Error>;
}

/// Classification after source-compatible tokenization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineAction {
    /// A whitespace-only line.
    Empty,
    /// An exact raw quit/help/version alias or stripped `fab` hint.
    Notice(ShellNotice),
    /// Tokens for the caller's command parser, without an executable prefix.
    Command(Vec<String>),
}

/// A submitted line could not be tokenized safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineError {
    /// The raw UTF-8 input exceeds the caller's limit.
    TooLong,
    /// The complete token list exceeds the caller's limit.
    TooManyTokens,
    /// A single or double quotation was not closed.
    NoClosingQuotation,
    /// A backslash was not followed by a character.
    NoEscapedCharacter,
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLong => "Interactive input exceeds the byte limit.",
            Self::TooManyTokens => "Interactive input exceeds the token limit.",
            Self::NoClosingQuotation => "No closing quotation",
            Self::NoEscapedCharacter => "No escaped character",
        })
    }
}

impl Error for LineError {}

/// Tokenizes `command.strip()` like Python `shlex.split(..., comments=False)`.
///
/// Only ASCII space, tab, CR, and LF split interior words. Unicode whitespace is
/// stripped at the outside like Python, but preserved inside words. Quotes
/// concatenate, empty quoted arguments survive, and double-quoted backslashes
/// only escape `"` and `\`. `#`, operators, substitutions, and redirects are
/// literal data, never executed by this crate. Malformed input is not truncated.
pub fn tokenize(line: &str, options: Options) -> Result<Vec<String>, LineError> {
    if line.len() > options.max_line_bytes {
        return Err(LineError::TooLong);
    }
    let mut chars = line.trim_matches(python_whitespace).chars();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' => {
                    let escaped = chars.next().ok_or(LineError::NoEscapedCharacter)?;
                    if !matches!(escaped, '\\' | '"') {
                        word.push('\\');
                    }
                    word.push(escaped);
                }
                _ => word.push(ch),
            },
            _ => match ch {
                ' ' | '\t' | '\r' | '\n' => {
                    if started {
                        push_word(&mut words, &mut word, options.max_tokens)?;
                        started = false;
                    }
                }
                '\'' | '"' => {
                    started = true;
                    quote = Some(ch);
                }
                '\\' => {
                    started = true;
                    word.push(chars.next().ok_or(LineError::NoEscapedCharacter)?);
                }
                _ => {
                    started = true;
                    word.push(ch);
                }
            },
        }
    }
    if quote.is_some() {
        return Err(LineError::NoClosingQuotation);
    }
    if started {
        push_word(&mut words, &mut word, options.max_tokens)?;
    }
    Ok(words)
}

fn push_word(words: &mut Vec<String>, word: &mut String, limit: usize) -> Result<(), LineError> {
    if words.len() >= limit {
        return Err(LineError::TooManyTokens);
    }
    words.push(std::mem::take(word));
    Ok(())
}

fn python_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'..='\u{000d}'
            | '\u{001c}'..='\u{0020}'
            | '\u{0085}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'..='\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Preserves raw, case-sensitive alias matching *after* lexical validation.
///
/// For example, `exit` quits, but ` exit `, `"exit"`, and `exit now` are
/// ordinary commands. Only the standalone `fab` hint uses stripped input.
pub fn classify_line(line: &str, options: Options) -> Result<LineAction, LineError> {
    let words = tokenize(line, options)?;
    let notice = match line {
        "quit" | "q" | "exit" => Some(ShellNotice::Exit),
        "help" | "h" | "-h" | "--help" => Some(ShellNotice::Help),
        "version" | "v" | "-v" | "--version" => Some(ShellNotice::Version),
        _ if line.trim_matches(python_whitespace) == "fab" => Some(ShellNotice::PrefixHint),
        _ => None,
    };
    Ok(match notice {
        Some(notice) => LineAction::Notice(notice),
        None if words.is_empty() => LineAction::Empty,
        None => LineAction::Command(words),
    })
}

/// A display prompt cannot be emitted within its byte limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptError {
    /// The original or escaped UTF-8 prompt exceeds the configured byte limit.
    TooLong,
}

impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Interactive prompt exceeds the byte limit.")
    }
}

impl Error for PromptError {}

/// Escapes ANSI/C0/C1 controls and directional/line separators in display data.
///
/// Printable context text, including `<`, `>`, Unicode, and `$`, remains literal.
/// The result has no terminal-control characters; it is never silently truncated.
pub fn safe_prompt(prompt: &str, max_bytes: usize) -> Result<String, PromptError> {
    if prompt.len() > max_bytes {
        return Err(PromptError::TooLong);
    }
    let mut safe = String::new();
    for ch in prompt.chars() {
        if ch.is_control()
            || matches!(
                ch,
                '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
        {
            for escaped in ch.escape_default() {
                safe.push(escaped);
            }
        } else {
            safe.push(ch);
        }
        if safe.len() > max_bytes {
            return Err(PromptError::TooLong);
        }
    }
    Ok(safe)
}

/// Successful, explicit loop termination, never an input/host error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// A raw quit alias was entered and the farewell was rendered.
    ExitCommand,
    /// Input ended; unlike the source EOF quirk, this is represented explicitly.
    EndOfInput,
    /// The application returned [`CommandOutcome::Stop`].
    HostRequested,
}

/// A fatal shell failure, preserving adapter and application error types.
#[derive(Debug)]
pub enum RunError<I, H> {
    /// Caller-provided limits are invalid.
    Options(OptionsError),
    /// Display data cannot be emitted safely within the prompt limit.
    Prompt(PromptError),
    /// The input adapter failed; this is not EOF or Ctrl+C.
    Input(I),
    /// Tokenization failed; like the source ValueError, this terminates the loop.
    Line(LineError),
    /// Application command, prompt, or notice handling failed.
    Host(H),
}

impl<I, H> fmt::Display for RunError<I, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Options(error) => fmt::Display::fmt(error, f),
            Self::Prompt(error) => fmt::Display::fmt(error, f),
            Self::Input(_) => f.write_str("Interactive input failed."),
            Self::Line(error) => fmt::Display::fmt(error, f),
            Self::Host(_) => f.write_str("Interactive command host failed."),
        }
    }
}

impl<I: Error + 'static, H: Error + 'static> Error for RunError<I, H> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::Options(error) => error,
            Self::Prompt(error) => error,
            Self::Input(error) => error,
            Self::Line(error) => error,
            Self::Host(error) => error,
        })
    }
}

/// Explicitly runs a synchronous shell over caller-owned input and commands.
///
/// No terminal is opened by this function itself. A fresh prompt is requested
/// before each event, including after interrupts and handled command failures.
/// EOF is consumed once with no misleading farewell/success notice. The source
/// inner exception handler printed `Error in interactive session: ` for EOF;
/// the caller can render that compatibility diagnostic for [`StopReason::EndOfInput`].
pub fn run<I: ShellInput, H: ShellHost>(
    input: &mut I,
    host: &mut H,
    options: Options,
) -> Result<StopReason, RunError<I::Error, H::Error>> {
    options.validate().map_err(RunError::Options)?;
    host.notice(ShellNotice::Welcome).map_err(RunError::Host)?;
    loop {
        let prompt = host.prompt().map_err(RunError::Host)?;
        let prompt = safe_prompt(&prompt, options.max_prompt_bytes).map_err(RunError::Prompt)?;
        match input.read_input(&prompt).map_err(RunError::Input)? {
            InputEvent::EndOfInput => return Ok(StopReason::EndOfInput),
            InputEvent::Interrupt => {
                host.notice(ShellNotice::InterruptHint)
                    .map_err(RunError::Host)?;
            }
            InputEvent::Line(line) => {
                match classify_line(&line, options).map_err(RunError::Line)? {
                    LineAction::Empty => {}
                    LineAction::Notice(notice) => {
                        host.notice(notice).map_err(RunError::Host)?;
                        if notice == ShellNotice::Exit {
                            return Ok(StopReason::ExitCommand);
                        }
                    }
                    LineAction::Command(tokens) => {
                        if host.execute(tokens).map_err(RunError::Host)? == CommandOutcome::Stop {
                            return Ok(StopReason::HostRequested);
                        }
                    }
                }
            }
        }
    }
}
