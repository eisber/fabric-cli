// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

//! Public analysis request, evidence and strictness contract tests.

use fabric_sdk_semantic_model::{
    AnalysisBackend, AnalysisError, AnalysisOutcome, AnalysisReport, AnalysisRequest, LABS_COMMIT,
    RULE_COUNT, Severity, analyze, rule_catalog,
};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

fn report(status: &str, rule_status: &str) -> Value {
    json!({
        "schemaVersion": "1",
        "source": {"commit": LABS_COMMIT},
        "status": status,
        "findings": [],
        "rules": rule_catalog().iter().map(|rule| json!({
            "id":rule.id,
            "ruleName":rule.name,
            "status":rule_status,
        })).collect::<Vec<_>>(),
        "coverage": {
            "total":RULE_COUNT,
            "evaluated":RULE_COUNT * usize::from(rule_status == "evaluated"),
            "unavailable":RULE_COUNT * usize::from(rule_status == "unavailable"),
            "failed":RULE_COUNT * usize::from(rule_status == "failed"),
        },
    })
}

fn decode(value: &Value) -> Result<AnalysisReport, AnalysisError> {
    AnalysisReport::from_json(&serde_json::to_vec(value).unwrap(), 65536)
}

#[test]
fn incomplete_rule_inputs_never_become_clean_scan() {
    let value = decode(&report("partial", "unavailable")).unwrap();
    assert_eq!(
        value.outcome(Severity::Error, false),
        AnalysisOutcome::Incomplete
    );
    assert!(matches!(
        decode(&report("succeeded", "unavailable")),
        Err(AnalysisError::InvalidResponse)
    ));
}

#[test]
fn object_failure_evidence_cannot_be_reported_as_a_complete_scan() {
    for detail in [
        json!({"failedObjects": 1}),
        json!({"unavailableObjects": 1}),
        json!({"issues": [{"status": "failed", "code": "predicate_error"}]}),
        json!({"issues": [{"status": "unavailable", "code": "evidence_missing"}]}),
        json!({"failedObjects": -1}),
        json!({"failedObjects": 0.5}),
        json!({"failedObjects": "1"}),
        json!({"unavailableObjects": null}),
        json!({"issues": {}}),
        json!({"issues": [null]}),
        json!({"issues": [{"status": "unknown"}]}),
    ] {
        let mut value = report("succeeded", "evaluated");
        value["rules"][0]
            .as_object_mut()
            .unwrap()
            .extend(detail.as_object().unwrap().clone());
        assert!(
            matches!(decode(&value), Err(AnalysisError::InvalidResponse)),
            "{detail}"
        );
    }
    for field in ["failedObjects", "unavailableObjects"] {
        let mut value = report("succeeded", "evaluated");
        value["coverage"][field] = json!(1);
        assert!(matches!(
            decode(&value),
            Err(AnalysisError::InvalidResponse)
        ));
    }
}

#[test]
fn consistent_object_counts_and_unknown_detail_are_preserved() {
    let mut value = report("succeeded", "evaluated");
    for rule in value["rules"].as_array_mut().unwrap() {
        rule["evaluatedObjects"] = json!(2);
        rule["unavailableObjects"] = json!(0);
        rule["failedObjects"] = json!(0);
        rule["issues"] = json!([]);
        rule["futureEvidence"] = json!({"retained": true});
    }
    value["coverage"]["evaluatedObjects"] = json!(RULE_COUNT * 2);
    value["coverage"]["unavailableObjects"] = json!(0);
    value["coverage"]["failedObjects"] = json!(0);
    let decoded = decode(&value).unwrap();
    assert_eq!(
        decoded.outcome(Severity::Info, true),
        AnalysisOutcome::Complete
    );
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["rules"][0]["futureEvidence"],
        json!({"retained": true})
    );
    value["coverage"]["evaluatedObjects"] = json!(RULE_COUNT * 2 + 1);
    assert!(matches!(
        decode(&value),
        Err(AnalysisError::InvalidResponse)
    ));
}

#[test]
fn explicit_failed_object_details_require_failed_rule_and_coverage_status() {
    let mut value = report("succeeded", "evaluated");
    for rule in value["rules"].as_array_mut().unwrap() {
        rule["evaluatedObjects"] = json!(0);
        rule["unavailableObjects"] = json!(0);
        rule["failedObjects"] = json!(0);
        rule["issues"] = json!([]);
    }
    value["rules"][0]["status"] = json!("failed");
    value["rules"][0]["failedObjects"] = json!(1);
    value["rules"][0]["issues"] = json!([{"status": "failed", "code": "predicate_error"}]);
    value["status"] = json!("failed");
    value["coverage"]["evaluated"] = json!(RULE_COUNT - 1);
    value["coverage"]["failed"] = json!(1);
    value["coverage"]["evaluatedObjects"] = json!(0);
    value["coverage"]["unavailableObjects"] = json!(0);
    value["coverage"]["failedObjects"] = json!(1);
    let decoded = decode(&value).unwrap();
    assert_eq!(
        decoded.outcome(Severity::Info, false),
        AnalysisOutcome::Incomplete
    );
    assert_eq!(decoded.rules[0].detail["failedObjects"], json!(1));
    value["coverage"]["failedObjects"] = json!(2);
    assert!(matches!(
        decode(&value),
        Err(AnalysisError::InvalidResponse)
    ));
}

#[test]
fn unavailable_model_issues_remain_incomplete_without_invented_object_counts() {
    let mut value = report("partial", "unavailable");
    for rule in value["rules"].as_array_mut().unwrap() {
        rule["evaluatedObjects"] = json!(0);
        rule["unavailableObjects"] = json!(0);
        rule["failedObjects"] = json!(0);
        rule["issues"] = json!([{"status": "unavailable", "code": "no_tables"}]);
    }
    value["coverage"]["evaluatedObjects"] = json!(0);
    value["coverage"]["unavailableObjects"] = json!(0);
    value["coverage"]["failedObjects"] = json!(0);
    let decoded = decode(&value).unwrap();
    assert_eq!(
        decoded.outcome(Severity::Info, false),
        AnalysisOutcome::Incomplete
    );
    assert_eq!(decoded.rules[0].detail["unavailableObjects"], json!(0));
}

#[test]
fn strict_findings_are_separate_from_runtime_failure() {
    let mut value = report("succeeded", "evaluated");
    value["findings"] = json!([{
        "category":"Performance", "ruleName":rule_catalog()[0].name, "severity":rule_catalog()[0].severity,
        "objectType":"Column", "objectName":"'Sales'[Amount]",
        "description":null, "url":null,
    }]);
    let report = decode(&value).unwrap();
    assert_eq!(
        report.outcome(Severity::Warning, true),
        AnalysisOutcome::Findings
    );
    assert_eq!(
        report.outcome(Severity::Error, true),
        AnalysisOutcome::Complete
    );
    assert_eq!(
        report.outcome(Severity::Info, false),
        AnalysisOutcome::Complete
    );
}

#[test]
fn downgraded_pinned_error_severity_cannot_bypass_strict_policy() {
    let error_rule = rule_catalog()
        .iter()
        .find(|rule| rule.name == "Data columns must have a source column")
        .unwrap();
    assert_eq!(error_rule.severity, Severity::Error);
    let mut value = report("succeeded", "evaluated");
    value["findings"] = json!([{
        "category":error_rule.category, "ruleName":error_rule.name, "severity":"Info",
        "objectType":error_rule.scopes[0], "objectName":"'Sales'[Amount]",
        "description":null, "url":null,
    }]);
    assert!(matches!(
        decode(&value),
        Err(AnalysisError::InvalidResponse)
    ));
}

#[test]
fn report_rejects_source_drift_duplicate_rules_and_wrong_counts() {
    let mut value = report("succeeded", "evaluated");
    value["source"]["commit"] = json!("different");
    assert!(decode(&value).is_err());
    let mut value = report("succeeded", "evaluated");
    value["coverage"]["total"] = json!(2);
    assert!(decode(&value).is_err());
    let mut value = report("succeeded", "evaluated");
    value["rules"].as_array_mut().unwrap().push(
        json!({"id":rule_catalog()[0].id,"ruleName":rule_catalog()[0].name,"status":"evaluated"}),
    );
    value["coverage"]["total"] = json!(RULE_COUNT + 1);
    value["coverage"]["evaluated"] = json!(RULE_COUNT + 1);
    assert!(decode(&value).is_err());
}

#[test]
fn omitted_rule_inventory_cannot_be_reported_as_complete() {
    let mut value = report("succeeded", "evaluated");
    value["rules"].as_array_mut().unwrap().pop();
    value["coverage"]["total"] = json!(RULE_COUNT - 1);
    value["coverage"]["evaluated"] = json!(RULE_COUNT - 1);
    assert!(matches!(
        decode(&value),
        Err(AnalysisError::InvalidResponse)
    ));
}

#[test]
fn substituted_and_mismatched_rule_identities_cannot_pass_completeness() {
    let mut value = report("succeeded", "evaluated");
    value["rules"][0]["id"] = json!("substituted-rule");
    assert!(matches!(
        decode(&value),
        Err(AnalysisError::InvalidResponse)
    ));
    let mut value = report("succeeded", "evaluated");
    value["rules"][0]["ruleName"] = json!("Different rule with a real ID");
    assert!(matches!(
        decode(&value),
        Err(AnalysisError::InvalidResponse)
    ));
}

#[test]
fn local_input_is_not_executable_tmsl_and_debug_omits_contents() {
    assert!(AnalysisRequest::bim(json!({"createOrReplace":{}}).to_string()).is_err());
    assert!(AnalysisRequest::bim(json!({"model":[], "password":"sensitive"}).to_string()).is_err());
    let request = AnalysisRequest::bim(
        json!({"compatibilityLevel":1702,"model":{"name":"private model"}}).to_string(),
    )
    .unwrap();
    assert!(!format!("{request:?}").contains("private model"));
    assert!(
        request
            .with_evidence(json!({"dependencies":[]}))
            .unwrap()
            .to_json()
            .is_ok()
    );
}

struct NeverCalled;
impl AnalysisBackend for NeverCalled {
    fn analyze(&self, _: &[u8], _: usize) -> Result<Vec<u8>, AnalysisError> {
        panic!("cancelled input must not invoke a backend");
    }
}

#[test]
fn cancellation_precedes_native_call() {
    let request = AnalysisRequest::bim(json!({"model":{}}).to_string()).unwrap();
    assert!(matches!(
        analyze(&NeverCalled, &request, &AtomicBool::new(true)),
        Err(AnalysisError::Cancelled)
    ));
}

#[test]
fn malformed_and_oversized_results_fail_explicitly() {
    assert!(matches!(
        AnalysisReport::from_json(b"{}", 65536),
        Err(AnalysisError::InvalidResponse)
    ));
    assert!(matches!(
        AnalysisReport::from_json(b"{}", 1),
        Err(AnalysisError::Limit)
    ));
}
