// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional binary-only glue. Shared commands never depend on the terminal editor.

use crate::{BatchState, CommandError, CommandOutcome, dispatch, write_error};
use fabric_cli::parser::{self, Invocation};
use fabric_cli_repl::{
    CommandOutcome as ShellOutcome, LineError, Options, RunError, ShellHost, ShellNotice,
    StopReason,
    editor::{EditorError, ReedlineEditor},
};
use std::{
    fmt,
    io::{self, Write},
    process::ExitCode,
};

pub(super) fn run() -> io::Result<ExitCode> {
    let options = Options::default();
    let mut editor = match ReedlineEditor::new(options) {
        Ok(editor) => editor,
        Err(EditorError::UnsupportedTerminal) => {
            return write_error(
                CommandError {
                    code: "InteractiveInputUnavailable",
                    message: "Interactive mode requires terminal stdin and stdout.".into(),
                },
                1,
            );
        }
        Err(_) => {
            return write_error(
                CommandError {
                    code: "InteractiveInputFailed",
                    message: "The interactive terminal editor could not start.".into(),
                },
                1,
            );
        }
    };
    let mut host = CommandShell {
        state: BatchState::capture_interactive(),
        output: io::stdout(),
        dispatch,
    };
    let result = fabric_cli_repl::run(&mut editor, &mut host, options);
    finish(result, &mut host.output)
}

fn finish<I>(
    result: Result<StopReason, RunError<I, HostError>>,
    output: &mut impl Write,
) -> io::Result<ExitCode> {
    match result {
        Ok(StopReason::ExitCommand | StopReason::HostRequested | StopReason::EndOfInput) => {
            Ok(ExitCode::SUCCESS)
        }
        Err(RunError::Line(
            error @ (LineError::NoClosingQuotation | LineError::NoEscapedCharacter),
        )) => {
            // Legacy lexical failures end the session normally after a diagnostic.
            writeln!(output, "Error in interactive session: {error}")?;
            Ok(ExitCode::SUCCESS)
        }
        Err(RunError::Host(HostError::Io(error))) => Err(error),
        Err(RunError::Host(HostError::InvalidContext)) => write_error(
            CommandError {
                code: "InvalidInput",
                message: "The working context must be an absolute Fabric path.".into(),
            },
            2,
        ),
        Err(RunError::Host(HostError::Terminal(exit))) => Ok(ExitCode::from(exit)),
        Err(error) => {
            writeln!(output, "Error in interactive session: {error}")?;
            Ok(ExitCode::from(1))
        }
    }
}

#[derive(Debug)]
enum HostError {
    Io(io::Error),
    InvalidContext,
    Terminal(u8),
}

impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Interactive command execution failed")
    }
}

impl std::error::Error for HostError {}

impl From<io::Error> for HostError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

type Dispatch = fn(Result<Invocation, clap::Error>, &mut BatchState) -> io::Result<CommandOutcome>;

struct CommandShell<W> {
    state: BatchState,
    output: W,
    dispatch: Dispatch,
}

impl<W: Write> ShellHost for CommandShell<W> {
    type Error = HostError;

    fn prompt(&mut self) -> Result<String, Self::Error> {
        let context = self.state.path().map_err(|_| HostError::InvalidContext)?;
        Ok(format!("fab:/{}$ ", context.trim_matches('/')))
    }

    fn execute(&mut self, tokens: Vec<String>) -> Result<ShellOutcome, Self::Error> {
        let Some(command) = tokens.first() else {
            return Ok(ShellOutcome::Continue);
        };
        if parser::command().find_subcommand(command).is_none() {
            write_error(
                CommandError {
                    code: "invalid_arguments",
                    message: "Unknown interactive command. Type 'help' for available commands."
                        .into(),
                },
                2,
            )?;
            return Err(HostError::Terminal(2));
        }
        let invocation =
            parser::parse_args(std::iter::once("fab-rs").chain(tokens.iter().map(String::as_str)));
        if let Err(error) = invocation {
            // The original shell contains subparser exits and keeps its session.
            (self.dispatch)(Err(error), &mut self.state)?;
            return Ok(ShellOutcome::Continue);
        }
        match (self.dispatch)(invocation, &mut self.state)? {
            CommandOutcome::Cancelled(_) => self.notice(ShellNotice::InterruptHint)?,
            CommandOutcome::Stop(exit) => return Err(HostError::Terminal(exit)),
            CommandOutcome::Returned(_) => {}
        }
        Ok(ShellOutcome::Continue)
    }

    fn notice(&mut self, notice: ShellNotice) -> Result<(), Self::Error> {
        match notice {
            ShellNotice::Help => writeln!(self.output, "{}", parser::command().render_long_help())?,
            ShellNotice::Version => {
                (self.dispatch)(parser::parse_args(["fab-rs", "version"]), &mut self.state)?;
            }
            other => {
                if let Some(message) = other.message() {
                    writeln!(self.output, "{message}")?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_cli_repl::{InputEvent, ShellInput};
    use std::{collections::VecDeque, convert::Infallible};

    struct Input(VecDeque<InputEvent>);
    impl ShellInput for Input {
        type Error = Infallible;
        fn read_input(&mut self, _: &str) -> Result<InputEvent, Self::Error> {
            Ok(self.0.pop_front().unwrap_or(InputEvent::EndOfInput))
        }
    }

    struct ScriptedInput(VecDeque<Result<InputEvent, io::Error>>);

    impl ShellInput for ScriptedInput {
        type Error = io::Error;

        fn read_input(&mut self, _: &str) -> Result<InputEvent, Self::Error> {
            self.0.pop_front().expect("unexpected extra editor read")
        }
    }

    #[test]
    fn scripted_session_child() {
        let Ok(case) = std::env::var("FAB_REPL_SESSION_TEST_CHILD") else {
            return;
        };
        let events = match case.as_str() {
            "eof" => vec![Ok(InputEvent::EndOfInput)],
            "interrupt-eof" => vec![Ok(InputEvent::Interrupt), Ok(InputEvent::EndOfInput)],
            "quit" | "exit" | "q" => vec![Ok(InputEvent::Line(case.clone()))],
            "read-error" => vec![Err(io::Error::other("synthetic editor failure"))],
            "host-stop" => vec![Ok(InputEvent::Line("get invalid-configuration".into()))],
            "quotation" => vec![Ok(InputEvent::Line("get \"unfinished".into()))],
            "escape" => vec![Ok(InputEvent::Line("get trailing\\".into()))],
            _ => panic!("unknown scripted session case"),
        };
        let mut input = ScriptedInput(
            events
                .into_iter()
                .chain([Ok(InputEvent::Line("must not execute".into()))])
                .collect(),
        );
        let mut host = CommandShell {
            state: BatchState::new_interactive(Ok(("/".into(), false))),
            output: io::stdout(),
            dispatch: synthetic_dispatch,
        };
        let result = fabric_cli_repl::run(&mut input, &mut host, Options::default());
        let exit = finish(result, &mut host.output).unwrap();
        assert_eq!(input.0.len(), 1);
        host.output.flush().unwrap();
        std::process::exit(if exit == ExitCode::SUCCESS {
            0
        } else if exit == ExitCode::from(2) {
            2
        } else {
            assert_eq!(exit, ExitCode::from(1));
            1
        });
    }

    #[test]
    fn scripted_sessions_preserve_exact_streams_and_exit_status() {
        let welcome = "\nWelcome to the Fabric CLI \u{26a1}\nType 'help' for help. \n\n";
        for (case, ending, exit) in [
            ("eof", "", 0),
            (
                "interrupt-eof",
                "\nUse 'quit' or 'exit' to leave interactive mode.\n",
                0,
            ),
            ("quit", "Exiting interactive mode. Goodbye!\n", 0),
            ("exit", "Exiting interactive mode. Goodbye!\n", 0),
            ("q", "Exiting interactive mode. Goodbye!\n", 0),
            (
                "read-error",
                "Error in interactive session: Interactive input failed.\n",
                1,
            ),
            ("host-stop", "", 2),
            (
                "quotation",
                "Error in interactive session: No closing quotation\n",
                0,
            ),
            (
                "escape",
                "Error in interactive session: No escaped character\n",
                0,
            ),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "repl_host::tests::scripted_session_child",
                    "--nocapture",
                    "--quiet",
                ])
                .env_clear()
                .env("FAB_REPL_SESSION_TEST_CHILD", case)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(exit), "{case}: {output:?}");
            assert_eq!(output.stderr, b"", "{case}");
            // The self-executed test harness prints its banner before the session.
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("\nrunning 1 test\n{welcome}{ending}"),
                "{case}",
            );
        }
    }

    #[test]
    fn fake_editor_uses_current_application_notices_without_terminal_setup() {
        let mut input = Input(VecDeque::from([
            InputEvent::Line("help".into()),
            InputEvent::Interrupt,
            InputEvent::Line("quit".into()),
        ]));
        let mut host = CommandShell {
            state: BatchState::new_interactive(Ok(("/".into(), false))),
            output: Vec::new(),
            dispatch,
        };
        let reason = fabric_cli_repl::run(&mut input, &mut host, Options::default()).unwrap();
        assert_eq!(reason, StopReason::ExitCommand);
        let text = String::from_utf8(host.output).unwrap();
        assert!(text.contains("Usage:"));
        assert!(text.contains("Use 'quit' or 'exit'"));
        assert!(text.contains("Goodbye!"));
    }

    #[test]
    fn interactive_navigation_does_not_inherit_command_line_persistence_mode() {
        let mut state = BatchState::new_interactive(Ok(("/".into(), false)));
        state.initialize_local().unwrap();
        assert!(!state.navigation.as_ref().unwrap().is_command_line());
    }

    #[test]
    fn invalid_context_is_a_configuration_failure_not_an_output_failure() {
        let mut host = CommandShell {
            state: BatchState::new_interactive(Err(fabric_cli_local::LocalError::command(
                "InvalidInput",
                "The working context must be an absolute Fabric path",
            ))),
            output: Vec::new(),
            dispatch,
        };
        let mut input = Input(VecDeque::from([InputEvent::Line("quit".into())]));
        assert!(matches!(
            fabric_cli_repl::run(&mut input, &mut host, Options::default()),
            Err(RunError::Host(HostError::InvalidContext)),
        ));
        assert_eq!(input.0.len(), 1);
    }

    #[test]
    fn malformed_quotes_and_escapes_stop_once_with_the_normal_session_exit_status() {
        for (line, diagnostic) in [
            ("get \"unfinished", "No closing quotation"),
            ("get trailing\\", "No escaped character"),
        ] {
            let mut input = Input(VecDeque::from([
                InputEvent::Line(line.into()),
                InputEvent::Line("quit".into()),
            ]));
            let mut host = CommandShell {
                state: BatchState::new_interactive(Ok(("/".into(), false))),
                output: Vec::new(),
                dispatch,
            };
            let result = fabric_cli_repl::run(&mut input, &mut host, Options::default());
            assert!(matches!(result, Err(RunError::Line(_))));
            assert_eq!(finish(result, &mut host.output).unwrap(), ExitCode::SUCCESS);
            assert_eq!(input.0.len(), 1);
            let output = String::from_utf8(host.output).unwrap();
            assert!(output.contains(&format!("Error in interactive session: {diagnostic}")));
            assert!(!output.contains("Goodbye!"));
        }
    }

    #[test]
    fn resource_limits_and_editor_failures_are_not_normal_lexical_termination() {
        for (error, diagnostic) in [
            (
                RunError::<io::Error, HostError>::Line(LineError::TooLong),
                "Interactive input exceeds the byte limit.",
            ),
            (
                RunError::Line(LineError::TooManyTokens),
                "Interactive input exceeds the token limit.",
            ),
            (
                RunError::Input(io::Error::other("Synthetic editor failure")),
                "Interactive input failed.",
            ),
        ] {
            let mut output = Vec::new();
            assert_eq!(finish(Err(error), &mut output).unwrap(), ExitCode::from(1));
            assert_eq!(
                output,
                format!("Error in interactive session: {diagnostic}\n").as_bytes(),
            );
        }
        let error =
            RunError::<io::Error, HostError>::Host(HostError::Io(io::ErrorKind::BrokenPipe.into()));
        assert_eq!(
            finish(Err(error), &mut Vec::new()).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe,
        );
    }

    struct FailingOutput {
        remaining: usize,
    }

    impl Write for FailingOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.remaining -= bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn welcome_and_farewell_output_failures_remain_errors_not_eof() {
        let welcome_bytes = ShellNotice::Welcome.message().unwrap().len() + 1;
        for (remaining, unread) in [(0, 2), (welcome_bytes, 1)] {
            let mut input = Input(VecDeque::from([
                InputEvent::Line("quit".into()),
                InputEvent::EndOfInput,
            ]));
            let mut host = CommandShell {
                state: BatchState::new_interactive(Ok(("/".into(), false))),
                output: FailingOutput { remaining },
                dispatch,
            };
            let result = fabric_cli_repl::run(&mut input, &mut host, Options::default());
            assert!(matches!(result, Err(RunError::Host(HostError::Io(_)))));
            assert_eq!(
                finish(result, &mut host.output).unwrap_err().kind(),
                io::ErrorKind::BrokenPipe,
            );
            assert_eq!(input.0.len(), unread);
        }
    }

    #[test]
    fn diagnostic_output_failures_are_propagated() {
        let result = Err(RunError::<io::Error, HostError>::Input(io::Error::other(
            "synthetic editor failure",
        )));
        assert_eq!(
            finish(result, &mut FailingOutput { remaining: 0 })
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe,
        );
    }

    #[test]
    fn typed_editor_failures_are_diagnostics_not_normal_eof() {
        for error in [
            EditorError::Read(io::ErrorKind::BrokenPipe.into()),
            EditorError::InputTooLong,
            EditorError::UnsupportedSignal,
        ] {
            let mut output = Vec::new();
            assert_eq!(
                finish(
                    Err(RunError::<EditorError, HostError>::Input(error)),
                    &mut output,
                )
                .unwrap(),
                ExitCode::from(1),
            );
            assert_eq!(
                output,
                b"Error in interactive session: Interactive input failed.\n",
            );
        }
    }

    fn synthetic_dispatch(
        invocation: Result<Invocation, clap::Error>,
        _: &mut BatchState,
    ) -> io::Result<CommandOutcome> {
        let Invocation::Read(command) = invocation.unwrap() else {
            panic!("expected the existing read parser to receive test tokens");
        };
        Ok(match command.path[0].as_str() {
            "auth-cancel" => CommandOutcome::Cancelled(1),
            "native-cancel" => CommandOutcome::Cancelled(2),
            "invalid-configuration" => CommandOutcome::Stop(2),
            other => panic!("unexpected synthetic path: {other}"),
        })
    }

    #[test]
    fn auth_and_native_cancellation_both_keep_context_and_accept_the_next_input() {
        for path in ["auth-cancel", "native-cancel"] {
            let mut input = Input(VecDeque::from([
                InputEvent::Line(format!("get {path}")),
                InputEvent::Line("quit".into()),
            ]));
            let mut host = CommandShell {
                state: BatchState::new_interactive(Ok(("/Demo.Workspace".into(), false))),
                output: Vec::new(),
                dispatch: synthetic_dispatch,
            };
            assert_eq!(
                fabric_cli_repl::run(&mut input, &mut host, Options::default()).unwrap(),
                StopReason::ExitCommand,
            );
            assert!(input.0.is_empty());
            assert_eq!(host.state.path().unwrap(), "/Demo.Workspace");
            let output = String::from_utf8(host.output).unwrap();
            assert!(output.contains("Use 'quit' or 'exit'"));
            assert!(output.contains("Goodbye!"));
        }
    }

    #[test]
    fn non_cancellation_exit_two_stops_without_an_interrupt_hint_or_next_command() {
        let mut input = Input(VecDeque::from([
            InputEvent::Line("get invalid-configuration".into()),
            InputEvent::Line("quit".into()),
        ]));
        let mut host = CommandShell {
            state: BatchState::new_interactive(Ok(("/Demo.Workspace".into(), false))),
            output: Vec::new(),
            dispatch: synthetic_dispatch,
        };
        assert!(matches!(
            fabric_cli_repl::run(&mut input, &mut host, Options::default()),
            Err(RunError::Host(HostError::Terminal(2))),
        ));
        assert_eq!(input.0.len(), 1);
        assert_eq!(host.state.path().unwrap(), "/Demo.Workspace");
        let output = String::from_utf8(host.output).unwrap();
        assert!(!output.contains("Use 'quit' or 'exit'"));
        assert!(!output.contains("Goodbye!"));
    }

    #[test]
    fn interrupted_delete_approval_stops_before_another_editor_read() {
        fn cancelled_delete(
            invocation: Result<Invocation, clap::Error>,
            _: &mut BatchState,
        ) -> io::Result<CommandOutcome> {
            assert!(matches!(invocation.unwrap(), Invocation::Delete(_)));
            Ok(crate::delete_host::failure_outcome(Some("Cancelled"), 1))
        }
        let mut input = Input(VecDeque::from([
            InputEvent::Line(
                "delete /11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook --permanent"
                    .into(),
            ),
            InputEvent::Line("quit".into()),
        ]));
        let mut host = CommandShell {
            state: BatchState::new_interactive(Ok(("/Demo.Workspace".into(), false))),
            output: Vec::new(),
            dispatch: cancelled_delete,
        };
        assert!(matches!(
            fabric_cli_repl::run(&mut input, &mut host, Options::default()),
            Err(RunError::Host(HostError::Terminal(1))),
        ));
        assert_eq!(input.0.len(), 1);
        assert_eq!(host.state.path().unwrap(), "/Demo.Workspace");
    }
}
