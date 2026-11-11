// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fake-editor/application integration and negative resource contracts.

#![deny(unnameable_test_items)]

use std::{collections::VecDeque, io};

use fabric_cli_repl::{
    CommandOutcome, InputEvent, LineError, Options, OptionsError, PromptError, RunError, ShellHost,
    ShellInput, ShellNotice, StopReason, run, safe_prompt, tokenize,
};

#[derive(Default)]
struct FakeInput {
    events: VecDeque<Result<InputEvent, io::Error>>,
    prompts: Vec<String>,
}

impl FakeInput {
    fn lines(lines: &[&str]) -> Self {
        Self {
            events: lines
                .iter()
                .map(|line| Ok(InputEvent::Line((*line).into())))
                .collect(),
            ..Self::default()
        }
    }
}

impl ShellInput for FakeInput {
    type Error = io::Error;

    fn read_input(&mut self, prompt: &str) -> Result<InputEvent, Self::Error> {
        self.prompts.push(prompt.to_owned());
        self.events
            .pop_front()
            .expect("unexpected extra input read")
    }
}

#[derive(Default)]
struct FakeHost {
    commands: Vec<Vec<String>>,
    notices: Vec<ShellNotice>,
    context: String,
    fail_prompt: bool,
    fail_notice: bool,
}

impl ShellHost for FakeHost {
    type Error = io::Error;

    fn prompt(&mut self) -> Result<String, Self::Error> {
        if self.fail_prompt {
            return Err(io::Error::other("prompt failure"));
        }
        Ok(format!("fab:/{}$ ", self.context))
    }

    fn execute(&mut self, tokens: Vec<String>) -> Result<CommandOutcome, Self::Error> {
        let command = tokens[0].clone();
        if command == "cd" {
            self.context.clone_from(&tokens[1]);
        }
        self.commands.push(tokens);
        match command.as_str() {
            "fatal" => Err(io::Error::other("synthetic host failure")),
            "stop" => Ok(CommandOutcome::Stop),
            // A real host renders its ordinary parser/command failure first.
            _ => Ok(CommandOutcome::Continue),
        }
    }

    fn notice(&mut self, notice: ShellNotice) -> Result<(), Self::Error> {
        if self.fail_notice {
            return Err(io::Error::other("presentation failure"));
        }
        self.notices.push(notice);
        Ok(())
    }
}

#[test]
fn fake_editor_dispatches_owned_tokens_and_refreshes_context_after_commands() {
    let mut input = FakeInput::lines(&[
        "cd '日本語.Workspace'",
        "get 'item.Notebook' -q \"a == 'b c'\"",
        "ordinary-parser-failure",
        "ls '' #literal",
        "quit",
    ]);
    let mut host = FakeHost::default();
    assert_eq!(
        run(&mut input, &mut host, Options::default()).unwrap(),
        StopReason::ExitCommand
    );
    assert_eq!(host.commands.len(), 4);
    assert_eq!(
        host.commands[1],
        ["get", "item.Notebook", "-q", "a == 'b c'"]
    );
    assert_eq!(host.commands[3], ["ls", "", "#literal"]);
    assert_eq!(input.prompts[0], "fab:/$ ");
    assert!(
        input.prompts[1..]
            .iter()
            .all(|prompt| prompt == "fab:/日本語.Workspace$ ")
    );
    assert_eq!(host.notices, [ShellNotice::Welcome, ShellNotice::Exit]);
}

#[test]
fn eof_stops_once_without_dispatch_notice_or_another_read() {
    let mut input = FakeInput {
        events: [
            Ok(InputEvent::EndOfInput),
            Ok(InputEvent::Line("must not execute".into())),
        ]
        .into(),
        ..FakeInput::default()
    };
    let mut host = FakeHost::default();
    assert_eq!(
        run(&mut input, &mut host, Options::default()).unwrap(),
        StopReason::EndOfInput
    );
    assert_eq!(input.prompts.len(), 1);
    assert_eq!(input.events.len(), 1);
    assert!(host.commands.is_empty());
    assert_eq!(host.notices, [ShellNotice::Welcome]);
}

#[test]
fn interrupt_continues_and_eof_stops_after_exactly_one_read_without_farewell() {
    let mut input = FakeInput {
        events: [
            Ok(InputEvent::Interrupt),
            Ok(InputEvent::EndOfInput),
            Ok(InputEvent::Line("must not execute".into())),
        ]
        .into(),
        ..FakeInput::default()
    };
    let mut host = FakeHost::default();
    assert_eq!(
        run(&mut input, &mut host, Options::default()).unwrap(),
        StopReason::EndOfInput
    );
    assert_eq!(input.prompts.len(), 2);
    assert_eq!(input.events.len(), 1);
    assert!(host.commands.is_empty());
    assert_eq!(
        host.notices,
        [ShellNotice::Welcome, ShellNotice::InterruptHint]
    );
}

#[test]
fn builtins_stay_out_of_command_dispatch_and_raw_whitespace_aliases_do_not() {
    let mut input = FakeInput::lines(&["help", "-v", " fab ", " ", " exit ", "q"]);
    let mut host = FakeHost::default();
    assert_eq!(
        run(&mut input, &mut host, Options::default()).unwrap(),
        StopReason::ExitCommand
    );
    assert_eq!(host.commands, [vec!["exit"]]);
    assert_eq!(
        host.notices,
        [
            ShellNotice::Welcome,
            ShellNotice::Help,
            ShellNotice::Version,
            ShellNotice::PrefixHint,
            ShellNotice::Exit,
        ]
    );
}

#[test]
fn read_errors_are_not_eof_or_success_and_do_not_retry() {
    let mut input = FakeInput {
        events: [Err(io::Error::other("synthetic read failure"))].into(),
        ..FakeInput::default()
    };
    let mut host = FakeHost::default();
    assert!(matches!(
        run(&mut input, &mut host, Options::default()),
        Err(RunError::Input(_))
    ));
    assert_eq!(input.prompts.len(), 1);
    assert_eq!(host.notices, [ShellNotice::Welcome]);
}

#[test]
fn lexical_errors_terminate_as_in_the_source_without_dispatch_or_next_read() {
    let mut input = FakeInput::lines(&["ls 'unclosed", "quit"]);
    let mut host = FakeHost::default();
    assert!(matches!(
        run(&mut input, &mut host, Options::default()),
        Err(RunError::Line(LineError::NoClosingQuotation))
    ));
    assert_eq!(input.prompts.len(), 1);
    assert!(host.commands.is_empty());
    assert_eq!(host.notices, [ShellNotice::Welcome]);
}

#[test]
fn fatal_host_errors_and_explicit_stop_are_not_conflated() {
    let mut input = FakeInput::lines(&["fatal", "quit"]);
    let mut host = FakeHost::default();
    assert!(matches!(
        run(&mut input, &mut host, Options::default()),
        Err(RunError::Host(_))
    ));
    assert_eq!(input.prompts.len(), 1);
    let mut input = FakeInput::lines(&["stop", "quit"]);
    assert_eq!(
        run(&mut input, &mut host, Options::default()).unwrap(),
        StopReason::HostRequested
    );
    assert_eq!(input.prompts.len(), 1);
    assert_eq!(host.notices, [ShellNotice::Welcome, ShellNotice::Welcome]);
}

#[test]
fn caller_prompt_and_notice_errors_propagate_before_reading_input() {
    let mut input = FakeInput::default();
    for (fail_prompt, fail_notice) in [(true, false), (false, true)] {
        let mut host = FakeHost {
            fail_prompt,
            fail_notice,
            ..FakeHost::default()
        };
        assert!(matches!(
            run(&mut input, &mut host, Options::default()),
            Err(RunError::Host(_))
        ));
        assert!(input.prompts.is_empty());
    }
}

#[test]
fn bytes_and_token_limits_reject_without_truncating_or_dispatching() {
    let options = Options {
        max_line_bytes: 4,
        max_tokens: 2,
        ..Options::default()
    };
    assert_eq!(tokenize("éé", options).unwrap(), ["éé"]);
    assert_eq!(tokenize("ééx", options), Err(LineError::TooLong));
    assert_eq!(tokenize("a b", options).unwrap(), ["a", "b"]);
    assert_eq!(
        tokenize(
            "'' '' ''",
            Options {
                max_tokens: 2,
                ..Options::default()
            }
        ),
        Err(LineError::TooManyTokens)
    );
    let mut input = FakeInput::lines(&["large", "q"]);
    let mut host = FakeHost::default();
    assert!(matches!(
        run(&mut input, &mut host, options),
        Err(RunError::Line(LineError::TooLong))
    ));
    assert!(host.commands.is_empty());
    assert_eq!(input.prompts.len(), 1);
}

#[test]
fn invalid_limits_fail_before_notices_or_reading() {
    for options in [
        Options {
            max_line_bytes: 0,
            ..Options::default()
        },
        Options {
            max_tokens: 0,
            ..Options::default()
        },
        Options {
            max_prompt_bytes: 0,
            ..Options::default()
        },
        Options {
            history_capacity: usize::MAX,
            ..Options::default()
        },
        Options {
            history_capacity: usize::MAX / 2,
            ..Options::default()
        },
    ] {
        let mut input = FakeInput::default();
        let mut host = FakeHost::default();
        assert!(matches!(
            run(&mut input, &mut host, options),
            Err(RunError::Options(_))
        ));
        assert!(input.prompts.is_empty());
        assert!(host.notices.is_empty());
    }
    assert_eq!(
        Options {
            max_tokens: 0,
            ..Options::default()
        }
        .validate(),
        Err(OptionsError::ZeroLimit)
    );
    assert!(
        Options {
            history_capacity: 0,
            ..Options::default()
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn prompt_controls_are_visible_data_and_expansion_is_byte_bounded() {
    assert_eq!(
        safe_prompt("fab:/日本語<x>\x1b[2J\n\r\0\u{009b}\u{202e}$ ", 200).unwrap(),
        "fab:/日本語<x>\\u{1b}[2J\\n\\r\\u{0}\\u{9b}\\u{202e}$ "
    );
    assert_eq!(safe_prompt("éé", 4).unwrap(), "éé");
    assert_eq!(
        safe_prompt("\u{061c}\u{200e}\u{200f}\u{2066}\u{2069}", 100).unwrap(),
        "\\u{61c}\\u{200e}\\u{200f}\\u{2066}\\u{2069}"
    );
    assert_eq!(safe_prompt("ééx", 4), Err(PromptError::TooLong));
    assert_eq!(safe_prompt("\x1b", 1), Err(PromptError::TooLong));
    let mut input = FakeInput::lines(&["q"]);
    let mut host = FakeHost {
        context: "\x1b[2J".into(),
        ..FakeHost::default()
    };
    run(&mut input, &mut host, Options::default()).unwrap();
    assert_eq!(input.prompts, ["fab:/\\u{1b}[2J$ "]);
}

#[test]
fn oversized_prompt_is_not_partially_displayed() {
    let mut input = FakeInput::default();
    let mut host = FakeHost::default();
    assert!(matches!(
        run(
            &mut input,
            &mut host,
            Options {
                max_prompt_bytes: 2,
                ..Options::default()
            }
        ),
        Err(RunError::Prompt(PromptError::TooLong))
    ));
    assert!(input.prompts.is_empty());
    assert!(host.commands.is_empty());
}
