// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser composition: authentication must not alter source read argv.

#![cfg(feature = "windows-broker")]

use fabric_cli::parser::{AuthAction, Invocation, ReadKind, parse_args};

#[test]
fn auth_values_named_like_reads_do_not_enter_the_read_lexical_adapter() {
    for action in ["login", "status", "logout"] {
        for account in ["ls", "dir", "get"] {
            let Invocation::Auth(command) = parse_args([
                "fab-rs",
                "auth",
                action,
                "--tenant",
                "tenant",
                "--account",
                account,
            ])
            .unwrap() else {
                panic!("expected auth, not read");
            };
            assert_eq!(command.account.as_deref(), Some(account));
            assert_eq!(command.tenant.as_deref(), Some("tenant"));
            assert_eq!(command.allow_interaction, action == "login");
            assert_eq!(command.timeout_seconds, 120);
            assert_eq!(command.client_id, "5814bfb4-2705-4994-b8d6-39aabeb5eaeb");
        }
    }
}

#[test]
fn omitted_auth_selectors_are_deferred_to_the_host_without_implicit_parser_io() {
    for action in ["login", "status", "logout"] {
        let Invocation::Auth(command) = parse_args(["fab-rs", "auth", action]).unwrap() else {
            panic!("expected auth");
        };
        assert!(command.tenant.is_none());
        assert!(command.account.is_none());
        assert!(!command.client_id_explicit);
        assert_eq!(command.allow_interaction, action == "login");
    }
    let Invocation::Auth(command) =
        parse_args(["fab-rs", "auth", "login", "--client-id", "explicit"]).unwrap()
    else {
        panic!("expected auth");
    };
    assert!(command.client_id_explicit);
}

#[test]
fn status_and_logout_are_never_interactive_and_login_can_disable_ui() {
    let Invocation::Auth(command) = parse_args([
        "fab-rs",
        "auth",
        "login",
        "--tenant",
        "tenant",
        "--account",
        "get",
        "--no-interactive",
        "--timeout-seconds",
        "1",
    ])
    .unwrap() else {
        panic!("expected auth");
    };
    assert_eq!(command.action, AuthAction::Login);
    assert!(!command.allow_interaction);
    assert_eq!(command.timeout_seconds, 1);
    for action in ["status", "logout"] {
        assert!(
            parse_args([
                "fab-rs",
                "auth",
                action,
                "--tenant",
                "tenant",
                "--account",
                "get",
                "--no-interactive",
            ])
            .is_err()
        );
    }
}

#[test]
fn read_paths_and_queries_named_auth_keep_the_existing_read_semantics() {
    for (name, kind) in [
        ("ls", ReadKind::List),
        ("dir", ReadKind::List),
        ("get", ReadKind::Get),
    ] {
        let Invocation::Read(command) =
            parse_args(["fab-rs", name, "auth", "--query", "'login'"]).unwrap()
        else {
            panic!("expected read");
        };
        assert_eq!(command.kind, kind);
        assert_eq!(command.path, ["auth"]);
        assert_eq!(
            command.query.as_deref(),
            Some(if kind == ReadKind::List {
                "'login'"
            } else {
                "login"
            })
        );
    }
}

#[test]
fn auth_accepts_output_format_at_every_global_option_position() {
    for action in ["login", "status", "logout"] {
        for format in ["json", "text"] {
            for args in [
                vec!["fab-rs", "--output_format", format, "auth", action],
                vec!["fab-rs", "auth", "--output_format", format, action],
                vec!["fab-rs", "auth", action, "--output_format", format],
            ] {
                let Invocation::Auth(command) = parse_args(args).unwrap() else {
                    panic!("expected auth");
                };
                assert_eq!(command.output_format.as_deref(), Some(format));
            }
        }
    }
    assert!(parse_args(["fab-rs", "auth", "status", "--output_format", "xml"]).is_err());
}
