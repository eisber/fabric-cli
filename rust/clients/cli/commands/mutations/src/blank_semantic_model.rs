// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Narrow access to the existing, source-pinned blank SemanticModel template.

use crate::MutationError;
use serde_json::Value;

/// A native create body containing only metadata and the pinned five-part template.
///
/// The template is shared with v1 creation. Its bytes, ordering, embedded `Blank`
/// name and zero logical ID are unchanged; only the outer display name is selected.
pub struct BlankSemanticModelCreate {
    payload: Value,
}

impl BlankSemanticModelCreate {
    /// Build blank creation metadata without accepting arbitrary model content.
    ///
    /// # Errors
    /// Rejects empty, surrounding-whitespace, control-bearing or path-like names,
    /// names longer than 256 characters, and descriptions over 256 characters.
    pub fn new(display_name: &str, description: Option<&str>) -> Result<Self, MutationError> {
        if display_name.is_empty()
            || display_name.trim() != display_name
            || display_name.chars().count() > 256
            || display_name.chars().any(char::is_control)
            || display_name.contains(['/', '\\', '?', '#', '%', '*'])
            || description.is_some_and(|value| value.chars().count() > 256)
        {
            return Err(MutationError::new(
                "InvalidInput",
                "Invalid blank SemanticModel creation metadata",
            ));
        }
        let mut payload = serde_json::json!({
            "displayName": display_name,
            "definition": crate::templates::semantic_model(None)?,
        });
        if let Some(description) = description {
            payload["description"] = description.into();
        }
        Ok(Self { payload })
    }

    /// Consume the typed body for the existing native item-mutation operation.
    pub fn into_payload(self) -> Value {
        self.payload
    }
}

#[cfg(test)]
#[deny(unnameable_test_items)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    #[test]
    fn template_is_the_existing_definition_with_every_pinned_byte_preserved() {
        let body = BlankSemanticModelCreate::new("Actual model", Some("Description"))
            .unwrap()
            .into_payload();
        assert_eq!(body["displayName"], "Actual model");
        assert_eq!(body["description"], "Description");
        assert_eq!(
            body["definition"],
            crate::templates::semantic_model(None).unwrap()
        );
        let files: &[(&str, &[u8])] = &[
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
        ];
        let parts = body["definition"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), files.len());
        for (part, (path, bytes)) in parts.iter().zip(files) {
            assert_eq!(part.as_object().unwrap().len(), 3);
            assert_eq!(part["path"], *path);
            assert_eq!(part["payloadType"], "InlineBase64");
            assert_eq!(part["payload"], STANDARD.encode(bytes));
            assert_eq!(
                STANDARD.decode(part["payload"].as_str().unwrap()).unwrap(),
                *bytes
            );
        }
        let platform: Value = serde_json::from_slice(
            &STANDARD
                .decode(parts[0]["payload"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(platform["metadata"]["displayName"], "Blank");
        assert_eq!(
            platform["config"]["logicalId"],
            "00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn omitted_and_empty_description_remain_distinct_and_names_are_literal() {
        for name in ["Model.with.dots", "11111111-1111-1111-1111-111111111111"] {
            let body = BlankSemanticModelCreate::new(name, None)
                .unwrap()
                .into_payload();
            assert_eq!(body["displayName"], name);
            assert!(body.get("description").is_none());
            assert_eq!(
                BlankSemanticModelCreate::new(name, Some(""))
                    .unwrap()
                    .into_payload()["description"],
                ""
            );
        }
    }

    #[test]
    fn invalid_metadata_does_not_build_a_definition() {
        for name in [
            "", " Model", "Model ", "a/b", "a\\b", "a\nb", "*", "a?b", "a%b",
        ] {
            assert!(BlankSemanticModelCreate::new(name, None).is_err());
        }
        assert!(BlankSemanticModelCreate::new(&"a".repeat(257), None).is_err());
        assert!(BlankSemanticModelCreate::new(&"a".repeat(256), None).is_ok());
        assert!(BlankSemanticModelCreate::new("Model", Some(&"a".repeat(257))).is_err());
        assert!(BlankSemanticModelCreate::new("Model", Some(&"a".repeat(256))).is_ok());
    }
}
