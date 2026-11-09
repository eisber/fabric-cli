// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline feedback preview, routing, privacy and payload-boundary contracts.
#![deny(unnameable_test_items)]

use fabric_cli_v2::{
    Status,
    report::{Destination, MAX_MESSAGE_BYTES, MAX_TITLE_BYTES, ReportRequest, prepare},
};
use serde_json::json;

fn request(destination: Destination) -> ReportRequest {
    ReportRequest {
        destination,
        title: "Caller supplied title".to_owned(),
        message: "Caller supplied body".to_owned(),
        handoff: false,
    }
}

#[test]
fn parses_only_explicit_supported_destinations() {
    assert_eq!(
        Destination::parse("github:Example-Org/repo.name_1-2").unwrap(),
        Destination::GitHub {
            owner: "Example-Org".to_owned(),
            repo: "repo.name_1-2".to_owned(),
        }
    );
    assert_eq!(
        Destination::parse("ideas").unwrap(),
        Destination::FabricIdeas
    );
    assert_eq!(
        Destination::parse("security").unwrap(),
        Destination::MicrosoftSecurity
    );
    let destination = format!("github:{}/{}", "a".repeat(39), "b".repeat(100));
    assert!(Destination::parse(&destination).is_ok());
}

#[test]
fn rejects_aliases_senders_and_uri_injection_without_fallback() {
    for destination in [
        "",
        "Ideas",
        "github",
        "send",
        "ideas --send",
        "https://github.com/owner/repo",
        " github:owner/repo",
        "github:owner/repo ",
        "github:/repo",
        "github:owner/",
        "github:-owner/repo",
        "github:owner-/repo",
        "github:ow--ner/repo",
        "github:ow_ner/repo",
        "github:owner/.",
        "github:owner/..",
        "github:owner/repo/issues",
        "github:owner\\repo",
        "github:owner/repo?body=secret",
        "github:owner/repo#fragment",
        "github:owner/repo@evil.example",
        "github:owner%2frepo/name",
        "github:owner/repo%2fissues",
        "github:owner/repo\n",
        "github:öwner/repo",
        "github:owner/répo",
    ] {
        let error = Destination::parse(destination).unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(error.effects.is_empty());
    }
    for destination in [
        format!("github:{}/repo", "a".repeat(40)),
        format!("github:owner/{}", "r".repeat(101)),
    ] {
        assert!(Destination::parse(&destination).is_err());
    }
}

#[test]
fn default_preview_is_exact_and_explicitly_not_delivered() {
    let result = prepare(&request(Destination::parse("github:owner/repo").unwrap())).unwrap();
    assert_eq!(result.status, Status::Prepared);
    assert!(result.effects.is_empty());
    assert_eq!(
        result.data,
        json!({
            "destination": {
                "kind": "github",
                "owner": "owner",
                "repo": "repo",
                "visibility": "unverified_repository_dependent",
            },
            "payload": {
                "title": "Caller supplied title",
                "body": "Caller supplied body",
            },
            "manual_route": "https://github.com/owner/repo/issues/new",
            "delivery": "not_sent",
            "instructions": "Nothing has been sent, saved or opened. Complete submission manually through the indicated route; review that provider's visibility and collection policy.",
            "warnings": ["Repository visibility is unverified and repository-dependent. Public-repository issues are public; private-repository issues are visible to permitted repository users. Labels do not provide confidentiality. Do not put sensitive data in a public issue."],
            "limits": {"title_bytes": 256, "message_bytes": 16384},
        })
    );
}

#[test]
fn explicit_handoff_changes_evidence_state_without_initiating_effects() {
    for destination in [
        Destination::parse("github:owner/repo").unwrap(),
        Destination::FabricIdeas,
        Destination::MicrosoftSecurity,
    ] {
        let mut input = request(destination);
        let draft = prepare(&input).unwrap();
        input.handoff = true;
        let handoff = prepare(&input).unwrap();
        assert_eq!(draft.status, Status::Prepared);
        assert_eq!(handoff.status, Status::Handoff);
        assert_eq!(draft.data, handoff.data);
        assert!(draft.effects.is_empty());
        assert!(handoff.effects.is_empty());
        assert_eq!(handoff.data["delivery"], "not_sent");
        assert!(handoff.data.get("receipt").is_none());
        assert!(handoff.data.get("approval").is_none());
    }
}

#[test]
fn manual_routes_follow_actual_ideas_and_microsoft_security_policy() {
    let ideas = prepare(&request(Destination::FabricIdeas)).unwrap();
    assert_eq!(
        ideas.data["manual_route"],
        "https://community.fabric.microsoft.com/t5/Ideas/ct-p/PBI_Comm_Ideas"
    );
    assert_eq!(ideas.data["destination"]["visibility"], "community_public");
    assert_eq!(
        ideas.data["warnings"],
        json!([
            "Fabric Ideas is community feedback, not a confidential channel. Do not include sensitive data."
        ])
    );
    let security = prepare(&request(Destination::MicrosoftSecurity)).unwrap();
    assert_eq!(security.data["manual_route"], "https://aka.ms/SECURITY.md");
    assert_eq!(
        security.data["destination"]["visibility"],
        "governed_by_microsoft_security_policy"
    );
    assert_eq!(
        security.data["warnings"],
        json!([
            "Follow Microsoft's security reporting policy for suspected vulnerabilities. Do not report vulnerabilities through public GitHub issues or Fabric Ideas."
        ])
    );
    assert_eq!(security.data["delivery"], "not_sent");
}

#[test]
fn direct_enum_construction_cannot_bypass_destination_validation() {
    let error = prepare(&request(Destination::GitHub {
        owner: "owner".to_owned(),
        repo: "repo?body=private".to_owned(),
    }))
    .unwrap_err();
    assert_eq!(error.code, "InvalidInput");
    assert!(!error.message.contains("private"));
    assert!(error.effects.is_empty());
}

#[test]
fn payload_preserves_unicode_whitespace_and_injection_as_data_not_url() {
    let mut input = request(Destination::parse("github:owner/repo").unwrap());
    input.title = "  Résumé 🦀 ?title=other&body=secret#fragment  ".to_owned();
    input.message = " \t{\"receipt\":\"not real\"}\r\n雪\n<&?token=synthetic> ".to_owned();
    let before = input.clone();
    let result = prepare(&input).unwrap();
    assert_eq!(input, before);
    assert_eq!(
        result.data["payload"],
        json!({"title": input.title, "body": input.message})
    );
    let route = result.data["manual_route"].as_str().unwrap();
    assert_eq!(route, "https://github.com/owner/repo/issues/new");
    assert!(!route.contains('?'));
    assert!(!route.contains('#'));
    assert!(result.data.get("receipt").is_none());
    assert!(result.effects.is_empty());
    let encoded = serde_json::to_string(&result).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded["data"]["payload"]["body"], input.message);
    assert_eq!(decoded["status"], "prepared");
}

#[test]
fn utf8_byte_limits_are_inclusive_not_character_limits() {
    let mut input = request(Destination::FabricIdeas);
    input.title = "a".repeat(MAX_TITLE_BYTES);
    input.message = "b".repeat(MAX_MESSAGE_BYTES);
    assert!(prepare(&input).is_ok());
    input.title.push('a');
    assert_eq!(prepare(&input).unwrap_err().code, "InvalidInput");
    input.title = "🦀".repeat(MAX_TITLE_BYTES / 4);
    input.message = "é".repeat(MAX_MESSAGE_BYTES / 2);
    assert!(prepare(&input).is_ok());
    input.title.push('é');
    assert!(prepare(&input).is_err());
    input.title = "Valid title".to_owned();
    input.message.push('é');
    assert!(prepare(&input).is_err());
}

#[test]
fn blank_or_control_bearing_payloads_fail_without_echoing_content() {
    for title in [
        "",
        "  ",
        "\u{2003}",
        "private\nvalue",
        "private\tvalue",
        "\0",
    ] {
        let mut input = request(Destination::FabricIdeas);
        input.title = title.to_owned();
        let error = prepare(&input).unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(!error.message.contains("private"));
        assert!(error.effects.is_empty());
    }
    for message in [
        "",
        " \n\r\t",
        "\u{2003}",
        "private\0value",
        "\u{1b}[2J",
        "\u{7f}",
    ] {
        let mut input = request(Destination::FabricIdeas);
        input.message = message.to_owned();
        let error = prepare(&input).unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(!error.message.contains("private"));
        assert!(error.effects.is_empty());
    }
}

#[test]
fn repeated_preparation_is_deterministic_without_enrichment_or_effects() {
    let input = request(Destination::MicrosoftSecurity);
    let first = prepare(&input).unwrap();
    let second = prepare(&input).unwrap();
    assert_eq!(first.status, second.status);
    assert_eq!(first.data, second.data);
    assert!(first.effects.is_empty());
    assert!(second.effects.is_empty());
    assert_eq!(first.data["payload"].as_object().unwrap().len(), 2);
    for forbidden in [
        "diagnostics",
        "timestamp",
        "tenant",
        "user",
        "receipt",
        "approval",
    ] {
        assert!(first.data.get(forbidden).is_none());
    }
}
