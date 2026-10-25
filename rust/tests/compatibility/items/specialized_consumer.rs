// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[tokio::test]
async fn public_source_collections_and_generic_consumers_coexist() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../commands/specialized-routes.json"
    )).unwrap();
    let routes = fixture["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 44);
    let credential = Arc::new(ConsumerCredential::default());
    let client = ConsumerClient::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let executor = ConsumerExecutor::default();
    for route in routes {
        let kind = route[0].as_str().unwrap();
        let collection = ItemCollection::from_item_type(kind).unwrap();
        assert_eq!(ItemCollection::from_item_type(&kind.to_lowercase()), Some(collection));
        assert_eq!(collection.as_str(), route[1].as_str().unwrap());
        let metadata = client.get_item_metadata_from_collection(
            &executor, "ws", collection, "opaque",
            ItemGetOptions {
                query: vec![
                    ("q!*~".into(), "old".into()),
                    ("other".into(), "a/b?&".into()),
                    ("q!*~".into(), "a b+雪".into()),
                ],
                ..Default::default()
            },
            pending(),
        ).await.unwrap();
        assert_eq!(metadata.id(), "opaque");
        // The route comes from context, not a possibly future response type.
        assert_eq!(metadata.item_type(), Some("Future"));
        assert_eq!(
            executor.0.lock().unwrap().last().unwrap(),
            &format!("https://api.fabric.microsoft.com/v1/workspaces/ws/{}/opaque?q%21%2A~=a+b%2B%E9%9B%AA&other=a%2Fb%3F%26", collection.as_str()),
        );
    }
    for unknown in ["", "Future", "notebooks", "../items", "https://evil.invalid", "Notebook/../items"] {
        assert_eq!(ItemCollection::from_item_type(unknown), None);
    }
    for id in ["", ".", "..", "x/y", "x\\y", "x?y", "x#y", "x\n"] {
        for (workspace, item) in [(id, "opaque"), ("ws", id)] {
            assert_eq!(
                client.get_item_metadata_from_collection(
                    &executor, workspace, ItemCollection::Notebook, item,
                    ItemGetOptions::default(), pending(),
                ).await.unwrap_err().kind,
                ItemErrorKind::InvalidIdentifier,
            );
        }
    }
    assert_eq!(credential.0.load(Ordering::SeqCst), 44);
    assert_eq!(executor.0.lock().unwrap().len(), 44);
    let generic = client.get_item_metadata(
        &executor, "ws", "opaque", ItemGetOptions::default(), pending(),
    ).await.unwrap();
    let typed = client.get_item(
        &executor, "ws", "opaque", ItemGetOptions::default(), pending(),
    ).await.unwrap();
    assert_eq!(generic.id(), typed.id);
    assert!(executor.0.lock().unwrap()[44..].iter().all(|url|
        url == "https://api.fabric.microsoft.com/v1/workspaces/ws/items/opaque"
    ));
}
