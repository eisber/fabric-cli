// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn all_origin_method_and_custom_header_branches_execute_shared_http_with_the_bound_token_scope()
 {
    for origin in [
        Origin::Fabric,
        Origin::OneLake,
        Origin::Arm,
        Origin::PowerBi,
    ] {
        for method in [
            Method::Get,
            Method::Post,
            Method::Patch,
            Method::Put,
            Method::Delete,
        ] {
            let mut arguments = options(&json!({}));
            arguments.origin = origin;
            arguments.method = method;
            arguments.headers = Some(
                "x-synthetic=first,X-Synthetic=last,content-type=application/custom+json".into(),
            );
            arguments.input = Some(ApiInput::Json(json!({"data":"synthetic"})));
            let (client, wire, credential) = setup_audience(
                vec![reply(403, r#"{"errorCode":"Denied"}"#, &[])],
                origin.audience(),
            );
            let result = api(
                &client,
                &arguments,
                NonZeroUsize::new(4096).unwrap(),
                pending(),
            )
            .await
            .unwrap();
            assert_eq!(result.data.unwrap()["status_code"], 403);
            assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
            let requests = wire.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method(), method);
            assert_eq!(
                requests[0].url().as_str(),
                format!("{}resources", origin.base_url())
            );
            assert_eq!(
                requests[0]
                    .headers()
                    .get_str(&"x-synthetic".into())
                    .unwrap(),
                "last"
            );
            assert_eq!(
                requests[0]
                    .headers()
                    .get_str(&"content-type".into())
                    .unwrap(),
                "application/custom+json"
            );
        }
    }
}

#[tokio::test]
async fn audience_mismatch_and_credential_headers_fail_before_token_or_network() {
    for origin in [
        Origin::Fabric,
        Origin::OneLake,
        Origin::Arm,
        Origin::PowerBi,
    ] {
        let mut arguments = options(&json!({}));
        arguments.origin = origin;
        for name in [
            "Authorization",
            "Host",
            "Cookie",
            "Set-Cookie",
            "Proxy-Authorization",
            "Content-Length",
            "Connection",
        ] {
            arguments.headers = Some(format!("{name}=forbidden"));
            let (client, wire, credential) = setup_audience(Vec::new(), origin.audience());
            assert!(
                api(
                    &client,
                    &arguments,
                    NonZeroUsize::new(100).unwrap(),
                    pending()
                )
                .await
                .is_err()
            );
            assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
            assert!(wire.requests.lock().unwrap().is_empty());
        }
        arguments.headers = None;
        let wrong = if origin.audience() == Audience::Arm {
            Audience::Fabric
        } else {
            Audience::Arm
        };
        let (client, wire, credential) = setup_audience(Vec::new(), wrong);
        assert_eq!(
            api(
                &client,
                &arguments,
                NonZeroUsize::new(100).unwrap(),
                pending()
            )
            .await
            .unwrap_err()
            .code,
            "InvalidInput"
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(wire.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn multipart_input_reaches_shared_executor_with_exact_framing_and_without_auth_override() {
    let mut arguments = options(&json!({}));
    arguments.method = Method::Post;
    arguments.input = Some(ApiInput::Multipart {
        file_name: "source.bin".into(),
        content: vec![0, 1, 2, 255],
        boundary: "synthetic-7196".into(),
    });
    let (client, wire, _) = setup(vec![reply(200, r#"{"uploaded":true}"#, &[])]);
    api(
        &client,
        &arguments,
        NonZeroUsize::new(1024).unwrap(),
        pending(),
    )
    .await
    .unwrap();
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let mut expected=b"--synthetic-7196\r\nContent-Disposition: form-data; name=\"file\"; filename=\"source.bin\"\r\n\r\n".to_vec();
    expected.extend([0, 1, 2, 255]);
    expected.extend_from_slice(b"\r\n--synthetic-7196--\r\n");
    assert_eq!(
        azure_core::Bytes::from(requests[0].body()).as_ref(),
        expected
    );
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"content-type".into())
            .unwrap(),
        "multipart/form-data; boundary=synthetic-7196"
    );
}
