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
            assert_eq!(command.account, account);
            assert_eq!(command.tenant, "tenant");
            assert_eq!(command.allow_interaction, action == "login");
            assert_eq!(command.timeout_seconds, 120);
            assert_eq!(command.client_id, "5814bfb4-2705-4994-b8d6-39aabeb5eaeb");
        }
    }
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
fn read_only_global_options_cannot_be_silently_ignored_by_auth() {
    for prefix in [vec!["--output_format", "json"], vec![]] {
        let mut args = vec!["fab-rs"];
        args.extend(prefix);
        args.extend(["auth", "login", "--tenant", "tenant", "--account", "get"]);
        args.extend(["--output_format", "json"]);
        assert!(parse_args(args).is_err());
    }
}
