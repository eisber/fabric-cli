// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline authentication contracts. No test creates a native broker or store.
#![deny(unnameable_test_items)]

use fabric_cli_v2::auth::{AuthAction, AuthRequest, DEFAULT_CLIENT_ID, run};

const TENANT: &str = "11111111-2222-3333-4444-555555555555";
const ACCOUNT: &str = "selected@example.test";
const SECRET: &str = "synthetic-secret-never-publish";

fn request() -> AuthRequest {
    AuthRequest {
        account: ACCOUNT.into(),
        tenant: TENANT.into(),
        client_id: DEFAULT_CLIENT_ID.into(),
        allow_interaction: false,
        timeout_seconds: 120,
    }
}

#[test]
fn explicit_request_validation_is_pure_bounded_and_sanitized() {
    assert_eq!(DEFAULT_CLIENT_ID, "5814bfb4-2705-4994-b8d6-39aabeb5eaeb");
    request().validate().unwrap();
    assert!(!format!("{:?}", request()).contains(ACCOUNT));
    assert!(!format!("{:?}", request()).contains(TENANT));
    for invalid in ["", "common", "organizations", SECRET] {
        let mut selected = request();
        selected.tenant = invalid.into();
        let error = selected.validate().unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(error.effects.is_empty());
        assert!(!serde_json::to_string(&error).unwrap().contains(SECRET));
    }
    for invalid in [
        "",
        "@example.test",
        "user@",
        "user@@example.test",
        "user name@example.test",
        "user\n@example.test",
        SECRET,
    ] {
        let mut selected = request();
        selected.account = invalid.into();
        assert_eq!(selected.validate().unwrap_err().code, "InvalidInput");
    }
    let mut selected = request();
    selected.account = format!("{}@example.test", "x".repeat(321));
    assert!(selected.validate().is_err());
    selected = request();
    selected.tenant = "9188040d-6c67-4c5b-b112-36a304b66dad".into();
    assert!(selected.validate().is_err());
    selected = request();
    selected.client_id = SECRET.into();
    assert!(selected.validate().is_err());
    for timeout in [0, 601, u64::MAX] {
        let mut selected = request();
        selected.timeout_seconds = timeout;
        assert!(selected.validate().is_err());
    }
    for timeout in [1, 600] {
        let mut selected = request();
        selected.timeout_seconds = timeout;
        selected.validate().unwrap();
    }
}

#[test]
fn invalid_configuration_precedes_feature_or_platform_access() {
    for action in [AuthAction::Login, AuthAction::Logout] {
        let mut selected = request();
        selected.account.clear();
        let error = run(action, &selected).unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(error.effects.is_empty());
    }
}

#[cfg(not(all(windows, feature = "windows-broker")))]
#[test]
fn unavailable_profile_fails_explicitly_without_a_provider_or_store() {
    for action in [AuthAction::Login, AuthAction::Logout] {
        let error = run(action, &request()).unwrap_err();
        assert_eq!(error.code, "UnsupportedCapability");
        assert!(error.effects.is_empty());
        assert!(!error.message.contains(ACCOUNT));
    }
}

#[cfg(feature = "windows-broker")]
mod injected {
    use super::*;
    use fabric_cli::auth_commands::{Broker, Failure, TokenMetadata};
    use fabric_cli_v2::{EffectEvidence, EffectKind, EffectState, Status, auth::run_with_broker};
    use std::{
        cell::{Cell, RefCell},
        future::{pending, ready},
        io,
    };
    use tokio::sync::Notify;

    const NOW: i64 = 1_700_000_000;

    fn metadata() -> Vec<TokenMetadata> {
        [
            "https://api.fabric.microsoft.com/.default",
            "https://storage.azure.com/.default",
            "https://management.azure.com/.default",
        ]
        .into_iter()
        .map(|scope| TokenMetadata {
            scope,
            expires_on: NOW + 3600,
        })
        .collect()
    }

    struct SyntheticBroker {
        tenant: String,
        account: String,
        metadata: Vec<TokenMetadata>,
        failure: Option<Failure>,
        interactions: RefCell<Vec<bool>>,
        logouts: Cell<usize>,
        dropped: Cell<usize>,
        switch_account: bool,
        wait: bool,
        started: Notify,
    }

    impl SyntheticBroker {
        fn new() -> Self {
            Self {
                tenant: TENANT.into(),
                account: ACCOUNT.into(),
                metadata: metadata(),
                failure: None,
                interactions: RefCell::new(Vec::new()),
                logouts: Cell::new(0),
                dropped: Cell::new(0),
                switch_account: false,
                wait: false,
                started: Notify::new(),
            }
        }

        async fn complete(&self) -> Result<(), Failure> {
            let _guard = Dropped(&self.dropped);
            self.started.notify_one();
            if self.wait {
                pending::<()>().await;
            }
            self.failure.clone().map_or(Ok(()), Err)
        }
    }

    struct Dropped<'a>(&'a Cell<usize>);

    impl Drop for Dropped<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    impl Broker for SyntheticBroker {
        fn tenant(&self) -> &str {
            &self.tenant
        }

        fn account(&self) -> &str {
            if self.switch_account && !self.interactions.borrow().is_empty() {
                "other@example.test"
            } else {
                &self.account
            }
        }

        async fn login(&self, allow_interaction: bool) -> Result<Vec<TokenMetadata>, Failure> {
            self.interactions.borrow_mut().push(allow_interaction);
            self.complete().await?;
            Ok(self.metadata.clone())
        }

        async fn status(&self) -> Result<Vec<TokenMetadata>, Failure> {
            panic!("v2 login must not be downgraded to status");
        }

        async fn logout(&self) -> Result<(), Failure> {
            self.logouts.set(self.logouts.get() + 1);
            self.complete().await
        }
    }

    #[tokio::test]
    async fn login_publishes_only_complete_metadata_and_explicit_interaction() {
        for interactive in [false, true] {
            let mut selected = request();
            selected.allow_interaction = interactive;
            let broker = SyntheticBroker::new();
            let result = run_with_broker(
                AuthAction::Login,
                &selected,
                &broker,
                pending(),
                pending(),
                || Some(NOW),
            )
            .await
            .unwrap();
            assert_eq!(result.status, Status::Succeeded);
            assert_eq!(result.data["logged_in"], true);
            assert_eq!(result.data["account"], ACCOUNT);
            assert_eq!(result.data["tenant_id"], TENANT);
            assert_eq!(result.data["client_id"], DEFAULT_CLIENT_ID);
            assert_eq!(result.data["mfa_evidence"], "not_checked");
            assert_eq!(result.data["service_access_verified"], false);
            assert_eq!(result.data["selection_persisted"], false);
            assert_eq!(result.data["tokens"].as_array().unwrap().len(), 3);
            assert_eq!(
                result.effects,
                [EffectEvidence::new(
                    EffectKind::Authentication,
                    EffectState::Completed
                )]
            );
            assert_eq!(*broker.interactions.borrow(), [interactive]);
            assert_eq!(broker.logouts.get(), 0);
            assert!(!serde_json::to_string(&result).unwrap().contains(SECRET));
        }
    }

    #[tokio::test]
    async fn logout_uses_only_the_scoped_signout_boundary_even_with_ui_permission() {
        let mut selected = request();
        selected.allow_interaction = true;
        let broker = SyntheticBroker::new();
        let result = run_with_broker(
            AuthAction::Logout,
            &selected,
            &broker,
            pending(),
            pending(),
            || panic!("logout must not require a clock"),
        )
        .await
        .unwrap();
        assert_eq!(result.data["signed_out"], true);
        assert_eq!(result.data["scope"], "application_on_this_device");
        assert_eq!(result.data["credential_invalidation"], "this_process");
        for key in [
            "selection_persisted",
            "already_returned_tokens_revoked",
            "other_processes_revoked",
            "os_account_removed",
            "browser_cookies_cleared",
        ] {
            assert_eq!(result.data[key], false);
        }
        assert_eq!(
            result.effects,
            [EffectEvidence::new(
                EffectKind::SignOut,
                EffectState::Completed
            )]
        );
        assert!(broker.interactions.borrow().is_empty());
        assert_eq!(broker.logouts.get(), 1);
    }

    #[tokio::test]
    async fn invalid_or_mismatched_selection_never_enters_broker_work() {
        for action in [AuthAction::Login, AuthAction::Logout] {
            for mismatch in ["configuration", "account", "tenant"] {
                let mut selected = request();
                let mut broker = SyntheticBroker::new();
                match mismatch {
                    "configuration" => selected.client_id = SECRET.into(),
                    "account" => broker.account = "different@example.test".into(),
                    _ => broker.tenant = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
                }
                let error =
                    run_with_broker(action, &selected, &broker, pending(), pending(), || {
                        Some(NOW)
                    })
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code,
                    if mismatch == "configuration" {
                        "InvalidInput"
                    } else {
                        "identity_mismatch"
                    }
                );
                assert!(error.effects.is_empty());
                assert!(broker.interactions.borrow().is_empty());
                assert_eq!(broker.logouts.get(), 0);
                assert!(!serde_json::to_string(&error).unwrap().contains(SECRET));
            }
        }
    }

    #[tokio::test]
    async fn incomplete_duplicate_reordered_unknown_or_expired_metadata_never_succeeds() {
        let mut cases = vec![
            Vec::new(),
            metadata()[..1].to_vec(),
            metadata()[..2].to_vec(),
        ];
        let mut duplicate = metadata();
        duplicate[2] = duplicate[1];
        cases.push(duplicate);
        let mut reordered = metadata();
        reordered.swap(0, 1);
        cases.push(reordered);
        let mut unknown = metadata();
        unknown[0].scope = SECRET;
        cases.push(unknown);
        for expires in [NOW, NOW - 1, -1, i64::MAX] {
            let mut expired = metadata();
            expired[2].expires_on = expires;
            cases.push(expired);
        }
        for tokens in cases {
            let mut broker = SyntheticBroker::new();
            broker.metadata = tokens;
            let error = run_with_broker(
                AuthAction::Login,
                &request(),
                &broker,
                pending(),
                pending(),
                || Some(NOW),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "invalid_broker_response");
            assert_eq!(
                error.effects,
                [EffectEvidence::new(
                    EffectKind::Authentication,
                    EffectState::Unknown
                )]
            );
            assert_eq!(broker.interactions.borrow().len(), 1);
            assert!(!serde_json::to_string(&error).unwrap().contains(SECRET));
        }
    }

    #[tokio::test]
    async fn identity_change_during_acquisition_never_publishes_success() {
        let mut broker = SyntheticBroker::new();
        broker.switch_account = true;
        let error = run_with_broker(
            AuthAction::Login,
            &request(),
            &broker,
            pending(),
            pending(),
            || Some(NOW),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "identity_mismatch");
        assert_eq!(error.effects[0].state, EffectState::Unknown);
        assert_eq!(broker.interactions.borrow().len(), 1);
        assert!(!serde_json::to_string(&error).unwrap().contains("other@"));
    }

    #[tokio::test]
    async fn clock_failure_after_acquisition_retains_possible_cache_effects() {
        for now in [None, Some(-1), Some(i64::MAX)] {
            let broker = SyntheticBroker::new();
            let error = run_with_broker(
                AuthAction::Login,
                &request(),
                &broker,
                pending(),
                pending(),
                || now,
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "clock_unavailable");
            assert_eq!(error.effects[0].state, EffectState::Unknown);
            assert_eq!(broker.interactions.borrow().len(), 1);
        }
    }

    #[tokio::test]
    async fn classified_failures_never_forward_provider_text_or_retry() {
        for action in [AuthAction::Login, AuthAction::Logout] {
            for code in [
                "invalid_configuration",
                "InvalidInput",
                "interaction_required",
                "identity_mismatch",
                "invalid_broker_response",
                "broker_unavailable",
                "cancelled",
                "Cancelled",
                "timed_out",
                "Timeout",
                "cleanup_pending",
                "completion_unconfirmed",
                "session_invalidated",
                "authentication_busy",
                "identity_capacity_exhausted",
                "broker_capacity_exhausted",
                "coordination_failed",
                SECRET,
            ] {
                let mut broker = SyntheticBroker::new();
                broker.failure = Some((code, SECRET.into()));
                let error =
                    run_with_broker(action, &request(), &broker, pending(), pending(), || {
                        Some(NOW)
                    })
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code,
                    match code {
                        "invalid_configuration" | "InvalidInput" => "InvalidInput",
                        "cancelled" | "Cancelled" => "Cancelled",
                        "timed_out" | "Timeout" => "Timeout",
                        SECRET => "authentication_failed",
                        _ => code,
                    }
                );
                let rendered = serde_json::to_string(&error).unwrap();
                assert!(!rendered.contains(SECRET));
                assert!(!rendered.contains(ACCOUNT));
                assert!(!rendered.contains(TENANT));
                assert_eq!(error.effects[0].state, EffectState::Unknown);
                assert_eq!(broker.interactions.borrow().len() + broker.logouts.get(), 1);
            }
        }
    }

    #[tokio::test]
    async fn cancellation_deadline_and_signal_failure_drop_work_without_false_completion() {
        for action in [AuthAction::Login, AuthAction::Logout] {
            for reason in ["cancelled", "timed_out", "signal_failed"] {
                let mut broker = SyntheticBroker::new();
                broker.wait = true;
                let error = run_with_broker(
                    action,
                    &request(),
                    &broker,
                    async {
                        if reason == "timed_out" {
                            pending().await
                        } else {
                            broker.started.notified().await;
                            if reason == "signal_failed" {
                                Err(io::Error::other(SECRET))
                            } else {
                                Ok(())
                            }
                        }
                    },
                    async {
                        if reason == "timed_out" {
                            broker.started.notified().await;
                        } else {
                            pending().await
                        }
                    },
                    || Some(NOW),
                )
                .await
                .unwrap_err();
                assert_eq!(
                    error.code,
                    match reason {
                        "cancelled" => "Cancelled",
                        "timed_out" => "Timeout",
                        _ => reason,
                    }
                );
                assert_eq!(broker.dropped.get(), 1);
                assert_eq!(error.effects[0].state, EffectState::Unknown);
                assert!(!serde_json::to_string(&error).unwrap().contains(SECRET));
                assert_eq!(broker.interactions.borrow().len() + broker.logouts.get(), 1);
            }
        }
    }

    #[tokio::test]
    async fn immediate_cancellation_never_claims_an_unstarted_effect() {
        let mut broker = SyntheticBroker::new();
        broker.wait = true;
        let error = run_with_broker(
            AuthAction::Login,
            &request(),
            &broker,
            ready(Ok(())),
            pending(),
            || Some(NOW),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "Cancelled");
        assert_eq!(error.effects.len(), broker.interactions.borrow().len());
        assert!(
            error
                .effects
                .iter()
                .all(|effect| effect.state == EffectState::Unknown)
        );
    }
}
