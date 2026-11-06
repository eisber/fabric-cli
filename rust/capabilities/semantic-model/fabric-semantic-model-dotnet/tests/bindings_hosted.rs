// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]
#![cfg(feature = "dotnet")]

use fabric_semantic_model_dotnet::{Analyzer, BindingError, CancellationToken, HostOptions};
use serde_json::json;

#[test]
fn missing_explicit_runtime_is_a_clear_failure() {
    assert_eq!(
        Analyzer::new(HostOptions::new("relative-hostfxr", "relative-component")).unwrap_err(),
        BindingError::InvalidConfiguration
    );
    let missing = std::env::current_dir()
        .unwrap()
        .join("hostfxr-does-not-exist");
    assert_eq!(
        Analyzer::new(HostOptions::new(&missing, &missing)).unwrap_err(),
        BindingError::RuntimeUnavailable
    );
}

#[test]
fn real_tom_repeated_calls_and_errors() {
    let hostfxr = std::env::var_os("FABRIC_BPA_TEST_HOSTFXR")
        .expect("feature-on hosted tests require an explicit trusted FABRIC_BPA_TEST_HOSTFXR");
    let options = HostOptions::bundled(hostfxr).unwrap();
    let analyzer = Analyzer::new(options.clone()).unwrap();
    let repeated = Analyzer::new(options.clone()).unwrap();
    let cancellation = CancellationToken::new();
    let request = json!({
        "schemaVersion": "1",
        "format": "bim",
        "model": "{\"name\":\"offline\",\"compatibilityLevel\":1600,\"model\":{\"culture\":\"en-US\"}}",
        "options": {},
        "evidence": {}
    });
    let bytes = serde_json::to_vec(&request).unwrap();
    for host in [&analyzer, &repeated, &analyzer.clone()] {
        let response = host.analyze_json(&bytes, &cancellation).unwrap();
        let output: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(output["schemaVersion"], "1");
        assert!(
            output["rules"]
                .as_array()
                .is_some_and(|rules| !rules.is_empty())
        );
        assert!(output["coverage"].is_object());
    }

    let mut malformed = request;
    malformed["model"] = json!("not a BIM database");
    assert_eq!(
        analyzer.analyze_json(&serde_json::to_vec(&malformed).unwrap(), &cancellation),
        Err(BindingError::MalformedModel)
    );
    cancellation.cancel();
    assert_eq!(
        analyzer.analyze_json(&bytes, &cancellation),
        Err(BindingError::Cancelled)
    );
    drop(analyzer);
    drop(repeated);
    let restored = Analyzer::new(options).unwrap();
    let worker_host = restored.clone();
    let worker_request = bytes.clone();
    let worker = std::thread::spawn(move || {
        worker_host.analyze_json(&worker_request, &CancellationToken::new())
    });
    assert!(
        restored
            .analyze_json(&bytes, &CancellationToken::new())
            .is_ok()
    );
    assert!(worker.join().unwrap().is_ok());
}
