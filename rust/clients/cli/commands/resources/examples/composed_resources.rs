// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Host composition without a parser, identity provider, runtime or live request.

use fabric_cli_output::Timestamp;
use fabric_cli_resources::{AclRead, CommandError, CommandValue, labels_from_bytes, read_acls};
use fabric_sdk_access::{AccessClient, Resource};
use fabric_sdk_api::Limits;

/// A host calls this on its existing runtime with its explicitly composed SDK.
pub async fn workspace_acls(client: &AccessClient, id: &str) -> Result<CommandValue, CommandError> {
    read_acls(
        client,
        &Resource::Workspace(id.into()),
        AclRead::List {
            long: true,
            query: None,
        },
        Limits::default(),
        std::future::pending(),
    )
    .await
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let value = labels_from_bytes(br#"{"labels":[{"name":"Public","id":"synthetic"}]}"#)?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = value.write_json(
        "label",
        Timestamp::parse("2000-01-01T00:00:00Z")?,
        &mut stdout,
        &mut stderr,
    )?;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(String::from_utf8(stdout)?.contains("Public"));
    Ok(())
}
