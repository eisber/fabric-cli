// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_cli_mutations::v2_link::{LinkEffects, LinkFailure, LinkOwner, LinkScope};

fn dispatch() -> host::Dispatch {
    host::Dispatch {
        result: Err(Box::new(LinkFailure {
            target: ResolvedLinkTarget::new(
                "11111111-1111-4111-8111-111111111111",
                "22222222-2222-4222-8222-222222222222",
                LinkOwner::DataAgent,
                LinkScope::DataAgentStagingDatasources,
            )
            .unwrap(),
            effects: LinkEffects {
                remote_write: true,
                query_acceleration: false,
                update_item_sensitivity: false,
            },
            error: MutationError::new("ExecutionFailed", "Synthetic unknown response"),
            observed: None,
        })),
        value: serde_json::json!({
            "status":"unknown", "mutationMayHaveOccurred":true,
            "operationId":"33333333-3333-4333-8333-333333333333",
            "remoteReplayed":false, "rollback":false
        }),
    }
}

#[tokio::test]
async fn original_deadline_prevents_output_and_preserves_reconciliation_evidence() {
    let original = dispatch();
    let expected = original.value.clone();
    let error = deliver(
        original,
        Instant::now() - Duration::from_secs(1),
        io::sink(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "TimedOut");
    assert_eq!(error.2, expected);
}

#[tokio::test]
async fn output_failure_retains_unknown_operation_evidence() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("Synthetic output failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let error = deliver(dispatch(), Instant::now() + Duration::from_secs(10), Broken)
        .await
        .unwrap_err();
    assert_eq!(error.0, "OutputFailed");
    assert_eq!(error.2["execution"]["status"], "unknown");
    assert_eq!(error.2["execution"]["mutationMayHaveOccurred"], true);
    assert_eq!(error.2["remoteReplayed"], false);
}
