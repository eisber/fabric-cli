// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Managed-header validation and privacy boundaries.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use fabric_core::{
    AgentKind, Audience, CLIENT_REQUEST_ID_HEADER, FABRIC_SKILL_HEADER, RequestCommand,
    RequestMetadata, USER_AGENT_HEADER, diagnostics::RequestId, validate_headers,
};
use std::collections::BTreeMap;

#[test]
fn metadata_is_bounded_redacted_and_cannot_be_spoofed_through_custom_headers() {
    let id = "00000000-0000-4000-8000-000000000001";
    let metadata = RequestMetadata::new(
        RequestCommand::Api,
        Some(AgentKind::Claude),
        RequestId::parse(id),
        Some("SKILL-CANARY"),
    )
    .unwrap();
    assert!(!format!("{metadata:?}").contains(id));
    assert!(!format!("{metadata:?}").contains("SKILL-CANARY"));
    for audience in [
        Audience::Fabric,
        Audience::Azure,
        Audience::Storage,
        Audience::PowerBi,
    ] {
        let mut headers = BTreeMap::new();
        metadata.apply(&mut headers, audience).unwrap();
        assert_eq!(headers[CLIENT_REQUEST_ID_HEADER], id);
        assert_eq!(
            headers.contains_key(FABRIC_SKILL_HEADER),
            audience == Audience::Fabric
        );
        assert_eq!(
            headers[USER_AGENT_HEADER],
            format!(
                "fabric-agent-cli/{} (api; {}; {}; claude-code)",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        );
    }
    for name in [
        USER_AGENT_HEADER,
        CLIENT_REQUEST_ID_HEADER,
        FABRIC_SKILL_HEADER,
    ] {
        for name in [name.to_owned(), name.to_uppercase()] {
            let error =
                validate_headers(&BTreeMap::from([(name, "SECRET-CANARY".into())])).unwrap_err();
            assert!(!error.message.contains("SECRET-CANARY"));
        }
    }
    for skill in ["\r\nCANARY", "\t", "\u{7f}", "café", &"x".repeat(1025)] {
        let error = RequestMetadata::new(RequestCommand::Api, None, None, Some(skill)).unwrap_err();
        assert!(!error.message.contains("CANARY"));
    }
    for skill in [None, Some(""), Some("   ")] {
        let metadata = RequestMetadata::new(RequestCommand::Api, None, None, skill).unwrap();
        let mut headers = BTreeMap::new();
        metadata.apply(&mut headers, Audience::Fabric).unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(
            headers[USER_AGENT_HEADER],
            format!(
                "fabric-agent-cli/{} (api; {}; {})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH,
            )
        );
    }
    let mut headers = BTreeMap::from([("accept".into(), "x".repeat(65530))]);
    validate_headers(&headers).unwrap();
    assert!(metadata.apply(&mut headers, Audience::Fabric).is_err());
}
