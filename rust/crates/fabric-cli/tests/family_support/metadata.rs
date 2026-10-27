// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::family_support::Harness;
use serde_json::{Value, json};

impl Harness {
    pub fn metadata(&self, items: Value) {
        self.metadata_with_id("w", items);
    }

    pub fn metadata_with_id(&self, workspace_id: &str, items: Value) {
        self.reply(
            200,
            json!({"value":[{"id":workspace_id,"displayName":"Demo","type":"Workspace"}]}),
        );
        self.reply(200, json!({"value":items}));
    }

    pub fn item(&self, kind: &str) {
        self.metadata(json!([{"id":"n","displayName":"Book","type":kind}]));
    }
}
