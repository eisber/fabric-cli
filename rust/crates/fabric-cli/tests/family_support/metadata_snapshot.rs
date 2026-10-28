// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::family_support::Harness;
use serde_json::{Value, json};

impl Harness {
    pub fn metadata_with_id(&self, workspace_id: &str, items: Value) {
        self.reply(
            200,
            json!({"value":[{"id":workspace_id,"displayName":"Demo","type":"Workspace"}]}),
        );
        self.reply(200, json!({"value":items}));
    }
}
