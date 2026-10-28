// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "metadata_snapshot.rs"]
mod snapshot;

use super::family_support::Harness;
use serde_json::{Value, json};

impl Harness {
    pub fn metadata(&self, items: Value) {
        self.metadata_with_id("w", items);
    }

    pub fn item(&self, kind: &str) {
        self.metadata(json!([{"id":"n","displayName":"Book","type":kind}]));
    }
}
