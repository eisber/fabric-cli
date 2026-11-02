// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

struct MetadataExecutor(&'static str);

impl ItemExecutor for MetadataExecutor {
    fn execute<'a>(&'a self, request: ItemRequest<'a>) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.url().as_str(), "https://api.fabric.microsoft.com/v1/workspaces/ws/items/opaque");
            Ok(ItemResponse {
                status: 200,
                body: self.0.as_bytes().to_vec(),
                headers: Default::default(),
            })
        })
    }
}

#[tokio::test]
async fn public_metadata_consumer_preserves_order_and_legacy_item_literals() {
    // Unknown properties precede known ones; duplicate keys follow actual JSON
    // parser semantics (last value, original key position), not a new SDK policy.
    let wire = r#"{"future":{"z":null,"a":[1,1.0,-0.0,9007199254740993]},"displayName":"\u96ea","id":"old","type":null,"id":"opaque","tail":false}"#;
    let expected = r#"{"future":{"z":null,"a":[1,1.0,-0.0,9007199254740993]},"displayName":"雪","id":"opaque","type":null,"tail":false}"#;
    let credential = Arc::new(ConsumerCredential::default());
    let client = ConsumerClient::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let metadata: ItemMetadata = client
        .get_item_metadata(&MetadataExecutor(wire), "ws", "opaque", ItemGetOptions::default(), pending())
        .await
        .unwrap();
    assert_eq!(metadata.id(), "opaque");
    assert_eq!(metadata.display_name(), "\u{96ea}");
    assert_eq!(metadata.item_type(), None);
    assert_eq!(metadata.as_value()["type"], serde_json::Value::Null);
    assert_eq!(serde_json::to_string(&metadata).unwrap(), expected);
    assert_eq!(serde_json::to_string(&metadata.clone().into_value()).unwrap(), expected);
    assert!(!format!("{metadata:?}").contains("opaque"));
    assert!(!format!("{metadata:?}").contains("future"));
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);

    let legacy = Item {
        id: "opaque".into(),
        display_name: "\u{96ea}".into(),
        additional_fields: [
            ("z".into(), serde_json::json!(1)),
            ("a".into(), serde_json::Value::Null),
        ].into(),
    };
    assert_eq!(
        serde_json::to_string(&legacy).unwrap(),
        "{\"id\":\"opaque\",\"displayName\":\"雪\",\"a\":null,\"z\":1}"
    );
    for (extra, item_type) in [
        ("", None),
        (r#","type":null"#, None),
        (r#","type":17"#, None),
        (r#","type":"Future""#, Some("Future")),
    ] {
        let metadata: ItemMetadata = serde_json::from_str(
            &format!(r#"{{"displayName":"","id":"opaque"{extra}}}"#),
        ).unwrap();
        assert_eq!(metadata.item_type(), item_type);
        assert_eq!(metadata.as_value().get("type").is_none(), extra.is_empty());
    }
    for invalid in [
        "null", "[]", "{}", r#"{"id":null,"displayName":"N"}"#,
        r#"{"id":"i","displayName":1}"#, r#"{"id":"i"}"#,
        r#"{"displayName":"N"}"#,
    ] {
        assert!(serde_json::from_str::<ItemMetadata>(invalid).is_err(), "{invalid}");
    }
}
