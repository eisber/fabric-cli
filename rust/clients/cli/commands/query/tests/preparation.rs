// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use azure_core::http::Url;
use fabric_cli_query_commands::{
    Artifact, DataAgentRoute, InputReader, InputSource, KqlRoute, PreparationError, QueryInput,
    QueryTarget, ResolvedRoute, Stage, TextSource, prepare_input,
};
use fabric_sdk_query::EngineBinding;
use fabric_v2_contracts::ItemIdentity;
use std::io;

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const AGENT: &str = "22222222-2222-2222-2222-222222222222";
const ARTIFACT: &str = "33333333-3333-3333-3333-333333333333";
const DATASOURCE: &str = "44444444-4444-4444-4444-444444444444";

fn artifact(item: &str) -> Artifact {
    Artifact {
        workspace: WORKSPACE.into(),
        item: item.into(),
    }
}

fn kql(engine: &str) -> KqlRoute {
    KqlRoute {
        engine: engine.into(),
        binding: EngineBinding::kusto(
            "fixture-binding",
            "v1",
            ItemIdentity::new(WORKSPACE, ARTIFACT).unwrap(),
            Url::parse("https://kusto.example.test/").unwrap(),
            "exact-database",
            "https://audience.example.test/.default",
            "fixture-account",
            "fixture-tenant",
        )
        .unwrap(),
    }
}

fn target() -> QueryTarget {
    QueryTarget {
        requested_artifact: artifact(AGENT),
        execution_artifact: artifact(ARTIFACT),
        data_agent: Some(DataAgentRoute {
            engine: "advertised-agent-engine".into(),
            agent: artifact(AGENT),
            datasource: DATASOURCE.into(),
            bound_artifact: artifact(ARTIFACT),
            stage: Stage::Sandbox,
        }),
        native_kql: vec![kql("advertised-native-engine")],
    }
}

#[derive(Default)]
struct Reader {
    bytes: Vec<u8>,
    calls: usize,
    stdin: bool,
    limit: usize,
    fail: bool,
}

impl InputReader for Reader {
    fn read(&mut self, source: InputSource<'_>, max_bytes: usize) -> io::Result<Vec<u8>> {
        self.calls += 1;
        self.stdin = matches!(source, InputSource::Stdin);
        self.limit = max_bytes;
        if self.fail {
            return Err(io::Error::other("private file and query contents"));
        }
        Ok(std::mem::take(&mut self.bytes))
    }
}

fn input(text: TextSource, parameters: Option<&str>) -> QueryInput {
    QueryInput {
        text,
        parameters: parameters.map(str::to_owned),
    }
}

#[test]
fn configured_data_agent_is_primary_and_preserves_datasource_and_stage() {
    let ResolvedRoute::AiHub(route) = target().resolve(None).unwrap() else {
        panic!("AIHub expected");
    };
    assert_eq!(route.agent, artifact(AGENT));
    assert_eq!(route.bound_artifact, artifact(ARTIFACT));
    assert_eq!(route.datasource, DATASOURCE);
    assert_eq!(route.stage, Stage::Sandbox);
}

#[test]
fn explicit_native_engine_selects_exact_binding_not_query_language() {
    let ResolvedRoute::Kql(route) = target().resolve(Some("advertised-native-engine")).unwrap()
    else {
        panic!("KQL expected");
    };
    assert_eq!(route.binding.database(), "exact-database");
    assert_eq!(route.binding.account(), "fixture-account");
    assert_eq!(route.binding.tenant(), "fixture-tenant");
    assert_eq!(
        route.binding.audience_scope(),
        "https://audience.example.test/.default"
    );
}

#[test]
fn bare_artifact_is_not_promoted_to_data_agent() {
    let mut target = target();
    target.data_agent = None;
    target.native_kql.clear();
    target.requested_artifact = target.execution_artifact.clone();
    assert_eq!(
        target.resolve(None).unwrap_err(),
        PreparationError::MissingProvider
    );
}

#[test]
fn native_artifact_remains_discoverable_without_data_agent() {
    let mut target = target();
    target.data_agent = None;
    target.requested_artifact = target.execution_artifact.clone();
    assert!(matches!(
        target.resolve(None).unwrap(),
        ResolvedRoute::Kql(_)
    ));
}

#[test]
fn unadvertised_engine_is_rejected_without_fallback() {
    for engine in ["kql", "jmespath", "AIHUB", "advertised-native-engine "] {
        assert!(target().resolve(Some(engine)).is_err());
    }
}

#[test]
fn ambiguous_native_bindings_fail_closed() {
    let mut target = target();
    target.data_agent = None;
    target.native_kql.push(kql("second-native-engine"));
    assert_eq!(
        target.resolve(None).unwrap_err(),
        PreparationError::AmbiguousProvider
    );
}

#[test]
fn duplicate_engine_identifiers_fail_explicit_selection() {
    let mut target = target();
    target.native_kql.push(kql("advertised-agent-engine"));
    assert_eq!(
        target.resolve(Some("advertised-agent-engine")).unwrap_err(),
        PreparationError::AmbiguousProvider
    );
}

#[test]
fn wrong_datasource_binding_or_fictitious_agent_is_rejected() {
    let mut target = target();
    target.data_agent.as_mut().unwrap().bound_artifact = artifact(AGENT);
    assert_eq!(
        target.resolve(None).unwrap_err(),
        PreparationError::InvalidTarget
    );
    target.data_agent.as_mut().unwrap().bound_artifact = artifact(ARTIFACT);
    target.data_agent.as_mut().unwrap().agent = artifact(ARTIFACT);
    assert_eq!(
        target.resolve(None).unwrap_err(),
        PreparationError::InvalidTarget
    );
}

#[test]
fn wrong_native_owner_is_rejected_before_provider_selection() {
    let mut target = target();
    target.execution_artifact = artifact(DATASOURCE);
    assert_eq!(
        target
            .resolve(Some("advertised-native-engine"))
            .unwrap_err(),
        PreparationError::InvalidTarget
    );
}

#[test]
fn inline_statement_is_opaque_and_reader_is_not_opened() {
    let route = target().resolve(None).unwrap();
    let mut reader = Reader::default();
    let text = "  // not parsed\nsome 'opaque' | input \r\n";
    let prepared = prepare_input(
        input(TextSource::Inline(text.into()), None),
        &route,
        &mut reader,
    )
    .unwrap();
    assert_eq!(prepared.text(), text);
    assert_eq!(reader.calls, 0);
    assert!(!format!("{prepared:?}").contains("opaque"));
}

#[test]
fn stdin_uses_exactly_one_bounded_reader_and_retains_text() {
    let route = target().resolve(None).unwrap();
    let mut reader = Reader {
        bytes: b"exact\r\ntext".to_vec(),
        ..Reader::default()
    };
    let prepared = prepare_input(
        input(TextSource::File("-".into()), None),
        &route,
        &mut reader,
    )
    .unwrap();
    assert_eq!(prepared.text(), "exact\r\ntext");
    assert_eq!(reader.calls, 1);
    assert!(reader.stdin);
    assert_eq!(reader.limit, 1024 * 1024);
}

#[test]
fn parameters_are_rejected_before_any_file_reader() {
    let route = target().resolve(None).unwrap();
    for parameters in ["parameters.json", "-"] {
        let mut reader = Reader::default();
        let error = prepare_input(
            input(TextSource::File("private.query".into()), Some(parameters)),
            &route,
            &mut reader,
        )
        .unwrap_err();
        assert_eq!(error, PreparationError::UnsupportedParameters);
        assert_eq!(reader.calls, 0);
    }
}

#[test]
fn two_stdin_sources_are_rejected_before_reader() {
    let mut reader = Reader::default();
    let error = prepare_input(
        input(TextSource::File("-".into()), Some("-")),
        &target().resolve(None).unwrap(),
        &mut reader,
    )
    .unwrap_err();
    assert_eq!(error, PreparationError::InvalidInput);
    assert_eq!(reader.calls, 0);
}

#[test]
fn invalid_input_and_reader_failure_never_echo_supplied_values() {
    let route = target().resolve(None).unwrap();
    for bytes in [vec![0xff], b" \r\n".to_vec(), b"x\0y".to_vec()] {
        let mut reader = Reader {
            bytes,
            ..Reader::default()
        };
        assert_eq!(
            prepare_input(
                input(TextSource::File("private.query".into()), None),
                &route,
                &mut reader,
            )
            .unwrap_err(),
            PreparationError::InvalidInput
        );
    }
    let mut reader = Reader {
        fail: true,
        ..Reader::default()
    };
    let error = prepare_input(
        input(TextSource::File("private.query".into()), None),
        &route,
        &mut reader,
    )
    .unwrap_err();
    assert_eq!(error, PreparationError::InputRead);
    assert!(!error.to_string().contains("private"));
}

#[test]
fn exact_statement_limit_is_accepted_and_excess_is_rejected() {
    let route = target().resolve(None).unwrap();
    let max = route.max_statement_bytes();
    for size in [max, max + 1] {
        let mut reader = Reader {
            bytes: vec![b'x'; size],
            ..Reader::default()
        };
        let result = prepare_input(
            input(TextSource::File("statement".into()), None),
            &route,
            &mut reader,
        );
        assert_eq!(result.is_ok(), size == max);
        assert_eq!(reader.calls, 1);
    }
}
