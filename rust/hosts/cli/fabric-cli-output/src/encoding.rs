// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::Indent;
use fabric_core_json::{Format, Indent as JsonIndent};
use serde_json::Value;

pub(crate) fn encode(value: &Value, indent: Indent) -> String {
    let indent = match indent {
        Indent::Zero => JsonIndent::Zero,
        Indent::Two => JsonIndent::Two,
        Indent::Four => JsonIndent::Four,
    };
    fabric_core_json::encode(value, Format::Pretty(indent))
}

pub(crate) fn encode_default(value: &Value) -> String {
    fabric_core_json::json_dumps(value)
}
