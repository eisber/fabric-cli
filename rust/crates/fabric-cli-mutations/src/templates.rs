// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native embedded definition bytes pinned to Python cf67fd52, never runtime files.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

pub(crate) fn semantic_model() -> Value {
    parts(&[
        (
            ".platform",
            include_bytes!("../templates/Blank.SemanticModel/.platform"),
        ),
        (
            "definition.pbism",
            include_bytes!("../templates/Blank.SemanticModel/definition.pbism"),
        ),
        (
            "definition/database.tmdl",
            include_bytes!("../templates/Blank.SemanticModel/definition/database.tmdl"),
        ),
        (
            "definition/model.tmdl",
            include_bytes!("../templates/Blank.SemanticModel/definition/model.tmdl"),
        ),
        (
            "definition/tables/Table.tmdl",
            include_bytes!("../templates/Blank.SemanticModel/definition/tables/Table.tmdl"),
        ),
    ])
}

pub(crate) fn generic_mirror() -> Value {
    parts(&[(
        "mirroring.json",
        include_bytes!("../templates/MirroredDatabase.GenericMirror/mirroring.json"),
    )])
}

fn parts(files: &[(&str, &[u8])]) -> Value {
    json!({"parts":files.iter().map(|(path,bytes)|json!({
        "path":path,"payload":STANDARD.encode(bytes),"payloadType":"InlineBase64",
    })).collect::<Vec<_>>()})
}
