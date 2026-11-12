// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Service-only opt-in mount borrowing the existing v2 native composition.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::{
    console::read_explicit_confirmation,
    v2_service_domain_host::{PreparedDomainSync, domain_selector},
};
use fabric_cli_resources::service::{
    self as command,
    service::v2::{ApprovalPolicy, PreparedOperation},
};
use std::{
    ffi::OsString,
    io::{self, IsTerminal},
    pin::pin,
    time::{Duration, Instant},
};

struct ImmediateApproval {
    expected: PreparedOperation,
    deadline: Instant,
    approved: bool,
}

impl ApprovalPolicy for ImmediateApproval {
    type Error = &'static str;

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        if !self.approved || operation != &self.expected || Instant::now() >= self.deadline {
            return Err("Exact Domain synchronization was not approved before the deadline.");
        }
        self.approved = false;
        Ok(())
    }
}

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let args: Vec<_> = std::iter::once(OsString::from("fab-cli-v2"))
        .chain(args)
        .collect();
    if let Err(error) = command::command().try_get_matches_from(&args) {
        if error.kind() == clap::error::ErrorKind::DisplayHelp {
            error
                .print()
                .map_err(|_| failed("OutputFailed", "Domain sync help delivery failed.", 1))?;
            return Ok(0);
        }
        return Err(failed(
            "InvalidArguments",
            "Invalid service arguments; run service domain sync --help.",
            2,
        ));
    }
    let command = command::parse_args(args)
        .map_err(|_| failed("InvalidArguments", "Invalid Domain sync arguments.", 2))?;
    domain_selector(&command.path)
        .map_err(|error| failed("InvalidTarget", error.to_string(), 2))?;
    if !io::stderr().is_terminal() {
        return Err(failed(
            "ApprovalRequired",
            "An interactive terminal is required for immediate Domain permission-change approval; no authentication or request occurred.",
            1,
        ));
    }
    let timeout = Duration::from_secs(120);
    let deadline = Instant::now() + timeout;
    let mode = std::env::var("FAB_RUST_AUTH_MODE")
        .map_err(|_| failed("NotConfigured", "Select an explicit FAB_RUST_AUTH_MODE.", 1))?;
    let endpoint = super::configured_endpoint()?;
    let selection = headless::Selection::capture(&mode)
        .map_err(|(code, message, exit)| failed(code, message, exit))?;
    let runtime = super::native_runtime()?;
    let result = runtime.block_on(async {
        let session = NativeSession::new(selection, endpoint, timeout)?;
        let mut interrupted = pin!(async {
            tokio::select! {
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => (),
                _ = tokio::signal::ctrl_c() => (),
            }
        });
        let prepared = PreparedDomainSync::prepare(
            &command,
            &session.context,
            session.executor,
            interrupted.as_mut(),
        )
        .await
        .map_err(|error| failed("DomainResolutionFailed", error.to_string(), 1))?;
        let operation = prepared.operation().clone();
        let context = prepared.context();
        let question = format!(
            "Synchronize Contributor role assignments to subdomains?\nTarget: {}\nDomain UUID: {}\nTenant: {}\nAccount: {}\nEndpoint: {}\nEffect: RemoteWrite; ExistingOnly; UnguardedConcurrency (no ETag).\nAdmin metadata was verified; current Fabric administrator and application admin controls remain service-enforced. OAuth scopes alone are not authority. Cancellation is not rollback; uncertain outcomes must not be replayed.\n",
            serde_json::to_string(operation.target().canonical_path()).expect("string JSON"),
            operation.input()["domainId"],
            serde_json::to_string(context.selection().tenant()).expect("string JSON"),
            serde_json::to_string(context.selection().account()).expect("string JSON"),
            context.endpoint().host(),
        );
        // A cancelled console worker cannot submit; this binary exits after the
        // bounded runtime shutdown rather than embedding the blocked input task.
        let confirmation = tokio::task::spawn_blocking(move || {
            #[cfg(windows)]
            let console = std::fs::File::open("CONIN$")?;
            #[cfg(not(windows))]
            let console = std::fs::File::open("/dev/tty")?;
            read_explicit_confirmation(
                &mut io::BufReader::new(console),
                &mut io::stderr(),
                &question,
            )
        });
        let approved = tokio::select! {
            biased;
            _ = interrupted.as_mut() => return Err(failed(
                "Cancelled", "Domain sync interrupted before approval; no POST occurred.", 1)),
            result = confirmation => result
                .map_err(|_| failed("ApprovalRequired", "Domain approval worker failed; no POST occurred.", 1))?
                .map_err(|_| failed("ApprovalRequired", "Domain approval was unavailable; no POST occurred.", 1))?,
        };
        prepared
            .execute(
                &session.context,
                &mut ImmediateApproval { expected: operation, deadline, approved },
                interrupted.as_mut(),
            )
            .await
            .map_err(|error| match error {
                command::ExecuteError::Approval(message) => failed("ApprovalDenied", message, 1),
                _ => failed("DomainSyncFailed", "Domain sync rejected local execution evidence; no replay was attempted.", 1),
            })
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let report = result?;
    let exit = command::exit_code(&report);
    command::write_report(report, &mut io::stdout().lock(), &mut io::stderr().lock())
        .map_err(|error| (
            "OutputFailed",
            "Domain sync receipt delivery failed; retain the executed evidence and do not replay.".into(),
            serde_json::json!({"receipt":error.report.receipt,"remoteReplayed":false}),
            1,
        ))?;
    Ok(exit)
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use command::service::{
        self, DomainTarget,
        v2::{Registry, ResolvedTarget, Target, TargetKind, TenantIdentity},
    };

    fn prepared(path: &str) -> PreparedOperation {
        let target = DomainTarget::new(
            ResolvedTarget::tenant_collection(
                path,
                TenantIdentity::new("11111111-1111-1111-1111-111111111111").unwrap(),
                Target {
                    kind: TargetKind::TenantCollection,
                    owner_type: "Domain".into(),
                    facet: "roleAssignments".into(),
                    representation: "native".into(),
                    generation: None,
                },
            )
            .unwrap(),
            "22222222-2222-2222-2222-222222222222",
        )
        .unwrap();
        let mut registry = Registry::default();
        service::register(&mut registry).unwrap();
        service::prepare(&registry, &target, "Contributor").unwrap()
    }

    #[test]
    fn native_approval_is_exact_immediate_and_single_use() {
        let operation = prepared("/.domains/Finance.Domain");
        for (approved, deadline) in [
            (false, Instant::now() + Duration::from_secs(10)),
            (true, Instant::now() - Duration::from_secs(1)),
        ] {
            let mut policy = ImmediateApproval {
                expected: operation.clone(),
                approved,
                deadline,
            };
            assert!(operation.authorize(&mut policy).is_err());
        }
        let mut policy = ImmediateApproval {
            expected: operation.clone(),
            approved: true,
            deadline: Instant::now() + Duration::from_secs(10),
        };
        assert!(
            prepared("/.domains/Other.Domain")
                .authorize(&mut policy)
                .is_err()
        );
        assert!(operation.authorize(&mut policy).is_ok());
        assert!(operation.authorize(&mut policy).is_err());
    }
}
