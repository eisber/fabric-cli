// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive terminal-result contracts from source main.py's exception boundary.
#![deny(unnameable_test_items)]

#[cfg(feature = "windows-broker")]
mod auth {
    use fabric_cli::{
        auth_commands::{self, Broker, Failure, TokenMetadata},
        parser::{
            AuthAction,
            batch::{self, BatchHost, BatchInvocation, CommandOutcome},
        },
    };
    use serde_json::Value;
    use std::{future::pending, io};

    struct SyntheticBroker(&'static str);

    impl Broker for SyntheticBroker {
        fn tenant(&self) -> &str {
            "synthetic-tenant"
        }
        fn account(&self) -> &str {
            "synthetic-account"
        }
        async fn login(&self, _: bool) -> Result<Vec<TokenMetadata>, Failure> {
            Err((self.0, "Synthetic broker boundary".into()))
        }
        async fn logout(&self) -> Result<(), Failure> {
            Ok(())
        }
    }

    struct Host {
        runtime: tokio::runtime::Runtime,
        broker: SyntheticBroker,
        changed: bool,
        summary: bool,
        stderr: Vec<u8>,
    }

    impl BatchHost for Host {
        type Error = io::Error;
        fn execute(&mut self, arguments: &[&str]) -> io::Result<CommandOutcome> {
            if arguments == ["mutate"] {
                self.changed = true;
                return Ok(CommandOutcome::Returned(0));
            }
            let result = self.runtime.block_on(auth_commands::run(
                AuthAction::Status,
                false,
                &self.broker,
                pending(),
                pending(),
            ));
            auth_commands::write_outcome(result, Vec::new(), &mut self.stderr)
        }
        fn separator(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn summary(&mut self, _: &BatchInvocation) -> io::Result<()> {
            self.summary = true;
            Ok(())
        }
    }

    #[test]
    fn existing_broker_cancellation_stops_but_same_exit_handled_failure_continues() {
        for (code, stops) in [("cancelled", true), ("authentication_failed", false)] {
            let mut host = Host {
                runtime: tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap(),
                broker: SyntheticBroker(code),
                changed: false,
                summary: false,
                stderr: Vec::new(),
            };
            let exit = batch::run(
                &BatchInvocation {
                    commands: vec!["auth".into(), "mutate".into()],
                    output_format: None,
                },
                &mut host,
            )
            .unwrap();
            assert_eq!(exit, u8::from(stops));
            assert_eq!(host.changed, !stops);
            assert_eq!(host.summary, !stops);
            let error: Value = serde_json::from_slice(&host.stderr).unwrap();
            assert_eq!(error["error"]["code"], code);
        }
    }

    #[test]
    fn timeout_signal_setup_and_output_failure_are_terminal_without_success() {
        for code in [
            "timed_out",
            "signal_failed",
            "cleanup_pending",
            "completion_unconfirmed",
        ] {
            assert_eq!(
                auth_commands::write_outcome(
                    Err((code, "Synthetic terminal boundary".into())),
                    Vec::new(),
                    Vec::new(),
                )
                .unwrap(),
                CommandOutcome::Stop(1)
            );
        }
        struct Broken;
        impl io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            auth_commands::write_outcome(
                Err(("cancelled", "Synthetic cancellation".into())),
                Vec::new(),
                Broken,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn both_auth_streams_preserve_the_underlying_writer_error_kind() {
        struct Broken(io::ErrorKind);
        impl io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(self.0.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for kind in [
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::WriteZero,
        ] {
            for result in [
                Ok(serde_json::json!({"logged_in": true})),
                Err(("authentication_failed", "Synthetic failure".into())),
            ] {
                assert_eq!(
                    auth_commands::write_outcome(result, Broken(kind), Broken(kind))
                        .unwrap_err()
                        .kind(),
                    kind
                );
            }
        }
    }
}
