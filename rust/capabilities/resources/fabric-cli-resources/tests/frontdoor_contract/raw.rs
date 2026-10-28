// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn parsed_raw_api_verbs_bodies_headers_queries_and_json_streams_match_unchanged_source() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/compatibility/raw_api/cases.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let mut args = vec![
            "api".into(),
            case["endpoint"].as_str().unwrap_or("resources").into(),
            "-X".into(),
            case["method"].as_str().unwrap_or("get").into(),
            "-A".into(),
            case["audience"].as_str().unwrap_or("fabric").into(),
        ];
        for (flag, field) in [("-P", "params"), ("-H", "headers"), ("-q", "query")] {
            if let Some(value) = case[field].as_str() {
                args.extend([flag.into(), value.into()]);
            }
        }
        if let Some(input) = case.get("input").filter(|input| !input.is_null()) {
            args.extend(["-i".into(), input.to_string()]);
        }
        if case["show_headers"] == true {
            args.push("--show_headers".into());
        }
        args.extend(["--output_format".into(), "json".into()]);
        let command = parsed(&args);
        let AccessKind::Api(options) = &command.kind else {
            panic!("raw API DTO")
        };
        let raw = case["raw_body"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| case["response"].to_string());
        let headers: Vec<_> = case["response_headers"]
            .as_object()
            .into_iter()
            .flat_map(|headers| headers.iter())
            .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
            .collect();
        let (api, wire, credential) = setup_audience(
            vec![reply(
                case["status"].as_u64().unwrap_or(200) as u16,
                &raw,
                &headers,
            )],
            options.origin.audience(),
        );
        let targets = target(None);
        let mut host = host(&api, &targets);
        host.clients.onelake = Some(&api);
        host.clients.arm = Some(&api);
        host.clients.powerbi = Some(&api);
        let result = host.execute(&command, pending()).await;
        let expected = &case["expected"];
        if !expected["error"].is_null() {
            let error = result.unwrap_err();
            assert_eq!(error.code, expected["error"]["code"].as_str().unwrap());
            assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
            assert!(wire.requests.lock().unwrap().is_empty());
            continue;
        }
        assert_eq!(
            result.as_ref().unwrap().data.as_ref().unwrap(),
            &expected["data"],
            "{}",
            case["name"]
        );
        let output = render(
            &command,
            result,
            Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            &FamilyPresentation::new(OutputFormat::Text, FamilySurface::Get),
            &ascii_width,
        )
        .unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            output.write_to(&mut stdout, &mut stderr).unwrap(),
            expected["exit"].as_u64().unwrap() as u8
        );
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            expected["stdout"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            expected["stderr"].as_str().unwrap()
        );
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].method().to_string(),
            expected["requests"][0]["method"].as_str().unwrap()
        );
        assert_eq!(
            requests[0].url().as_str(),
            expected["requests"][0]["url"].as_str().unwrap()
        );
        let bytes = azure_core::Bytes::from(requests[0].body());
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice::<Value>(&bytes).unwrap()
        };
        assert_eq!(body, expected["requests"][0]["body"]);
        for (key, value) in expected["requests"][0]["headers"].as_object().unwrap() {
            assert_eq!(
                requests[0].headers().get_str(&key.clone().into()).unwrap(),
                value.as_str().unwrap()
            );
        }
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    }
}
