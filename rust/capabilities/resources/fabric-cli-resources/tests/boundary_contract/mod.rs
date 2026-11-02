// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod completion;
mod mutation_edges;
mod wire;
use super::*;
use fabric_cli_resources::{
    AclExportSink, CommandError, CommandValue, NamedResource, export_acls, read_named_acls,
};
use std::pin::Pin;

#[derive(Default)]
struct Sink {
    exports: Vec<Value>,
}
impl AclExportSink for Sink {
    fn export<'a>(
        &'a mut self,
        name: &'a str,
        data: &'a Value,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<CommandValue, CommandError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.exports.push(json!({"name":name,"data":data}));
            Ok(CommandValue::default())
        })
    }
}

#[tokio::test]
async fn source_export_intents_include_falsey_values_and_preserve_query_and_resource_name() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/compatibility/access/boundaries.json"
    ))
    .unwrap();
    for case in fixture["exports"].as_array().unwrap() {
        let (api, wire, _) = setup(vec![reply(200, &case["body"].to_string(), &[])]);
        let target = NamedResource {
            resource: resource("workspace"),
            name: "Resource".into(),
        };
        let mut sink = Sink::default();
        let result = export_acls(
            &AccessClient::new(api),
            &target,
            case["query"].as_str(),
            Limits::default(),
            &mut sink,
            pending(),
        )
        .await
        .unwrap();
        assert!(result.data.is_none());
        assert_eq!(
            json!(sink.exports),
            case["expected"]["exports"],
            "{}",
            case["name"]
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn source_onelake_guidance_and_pre_request_warnings_remain_distinct_from_authorization_errors()
 {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/compatibility/access/boundaries.json"
    ))
    .unwrap();
    for case in fixture["errors"].as_array().unwrap() {
        let code = case["code"].as_str().unwrap();
        let (api, wire, _) = setup(vec![reply(
            if code == "BadRequest" { 400 } else { 403 },
            &json!({"errorCode":code,"message":"source service message"}).to_string(),
            &[],
        )]);
        let target = NamedResource {
            resource: resource("onelake"),
            name: "Resource.Lakehouse".into(),
        };
        let options = if case["command"] == "ls" {
            AclRead::List {
                long: false,
                query: Some(".".into()),
            }
        } else {
            AclRead::Get {
                query: Some(".".into()),
            }
        };
        let e = read_named_acls(
            &AccessClient::new(api),
            &target,
            Some("Workspace.Workspace"),
            options,
            Limits::default(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, case["expected"]["error"]["code"].as_str().unwrap());
        assert_eq!(
            e.message,
            case["expected"]["error"]["message"].as_str().unwrap()
        );
        assert_eq!(json!(e.warnings), case["expected"]["warnings"]);
        assert_eq!(json!(e.diagnostics), case["expected"]["diagnostics"]);
        assert_eq!(e.handled, case["expected"]["handled"].as_bool().unwrap());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = write_error(
            &e,
            "acl",
            Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(exit, if e.handled { 0 } else { 1 });
        assert_eq!(
            serde_json::from_slice::<Value>(&out).unwrap()["status"],
            "Failure"
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}
