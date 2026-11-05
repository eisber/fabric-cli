// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated authored inputs. Served projections are deliberately not inputs.

use crate::{ValidationError, models::ItemReference, selector, text_bound};
use serde::Serialize;

/// Reference-only attachment request; it never creates or edits the ontology.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachOntology {
    ontology_reference: ItemReference,
    display_name: String,
}

impl AttachOntology {
    /// Validates the mandatory safe storage display name.
    pub fn new(
        ontology_reference: ItemReference,
        display_name: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let display_name = display_name.into();
        if display_name.trim().is_empty()
            || display_name != display_name.trim()
            || display_name.ends_with('.')
            || display_name.contains(['/', '\\'])
        {
            return Err(ValidationError::InvalidDisplayName);
        }
        Ok(Self {
            ontology_reference,
            display_name,
        })
    }

    /// Returns the referenced ontology identity, not the agent route identity.
    pub fn reference(&self) -> &ItemReference {
        &self.ontology_reference
    }
}

/// Native entity-only selection; omitted IDs explicitly mean all entities.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectEntities {
    #[serde(skip_serializing_if = "Option::is_none")]
    element_ids: Option<Vec<String>>,
    is_selected: bool,
}

impl SelectEntities {
    /// Explicitly applies to all entities, without manufacturing an empty ID list.
    pub fn all(is_selected: bool) -> Self {
        Self {
            element_ids: None,
            is_selected,
        }
    }

    /// Uses returned opaque entity IDs as-is; grouping/schema IDs remain server-rejected.
    pub fn named(element_ids: Vec<String>, is_selected: bool) -> Result<Self, ValidationError> {
        if element_ids.is_empty() || element_ids.len() > 500 {
            return Err(ValidationError::InvalidSelection);
        }
        for id in &element_ids {
            selector(id, 8192)?;
        }
        Ok(Self {
            element_ids: Some(element_ids),
            is_selected,
        })
    }

    /// Expected per-element result order for explicit selection.
    pub fn element_ids(&self) -> Option<&[String]> {
        self.element_ids.as_deref()
    }
}

/// Partial authored source update. Omitted fields are preserved by the server.
///
/// The source ignores null on PATCH; clearing instructions uses DELETE, while
/// clearing omitted instructions/description together uses explicit PUT.
#[derive(Clone, Debug, Serialize)]
pub struct InstructionsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

impl InstructionsPatch {
    /// Validates UTF-16 lengths used by the source controller.
    pub fn new(
        instructions: Option<String>,
        description: Option<String>,
    ) -> Result<Self, ValidationError> {
        if let Some(value) = &instructions {
            text_bound(value, 15_000)?;
        }
        if let Some(value) = &description {
            text_bound(value, 500_000)?;
        }
        Ok(Self {
            instructions,
            description,
        })
    }
}

/// Explicit replacement; omitted fields clear their stored values.
#[derive(Clone, Debug, Serialize)]
pub struct InstructionsReplace {
    instructions: Option<String>,
    description: Option<String>,
}

impl InstructionsReplace {
    /// Creates a replacement distinct from a patch.
    pub fn new(
        instructions: Option<String>,
        description: Option<String>,
    ) -> Result<Self, ValidationError> {
        InstructionsPatch::new(instructions.clone(), description.clone())?;
        Ok(Self {
            instructions,
            description,
        })
    }
}

/// Authored example creation; query text is data, never executed by this SDK.
///
/// Blank questions are valid source drafts and retain their service validation
/// state. Creating a draft does not certify it as valid for runtime retrieval.
#[derive(Clone, Debug, Serialize)]
pub struct FewShotCreate {
    question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
}

impl FewShotCreate {
    /// Validates source-defined text limits, preserving blank drafts.
    pub fn new(question: String, query: Option<String>) -> Result<Self, ValidationError> {
        text_bound(&question, 1000)?;
        if let Some(value) = &query {
            text_bound(value, 5000)?;
        }
        Ok(Self { question, query })
    }
}

/// Partial authored example update; omitted/null fields are not replacement input.
#[derive(Clone, Debug, Serialize)]
pub struct FewShotPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
}

impl FewShotPatch {
    /// Validates without filling absent fields from a served projection.
    pub fn new(question: Option<String>, query: Option<String>) -> Result<Self, ValidationError> {
        if let Some(value) = &question {
            text_bound(value, 1000)?;
        }
        if let Some(value) = &query {
            text_bound(value, 5000)?;
        }
        Ok(Self { question, query })
    }
}

/// Explicit whole-agent publish request, never attached to a staging edit.
#[derive(Clone, Debug, Serialize)]
pub struct Publish {
    #[serde(
        rename = "publishedDescription",
        skip_serializing_if = "Option::is_none"
    )]
    description: Option<String>,
}

impl Publish {
    /// Preserves optional description and validates source UTF-16 bounds.
    pub fn new(description: Option<String>) -> Result<Self, ValidationError> {
        if let Some(value) = &description {
            text_bound(value, 500_000)?;
        }
        Ok(Self { description })
    }
}

/// Explicit schema-cache policy, not a hidden live refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseSource {
    /// Cache-first, potentially stale according to server policy.
    Any,
    /// Explicitly request fresh discovery and cache updates.
    Live,
}

impl ResponseSource {
    /// Exact service query spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Live => "live",
        }
    }
}

/// Bounded single-level element paging. IDs/tokens are opaque, not decoded paths.
#[derive(Clone, Debug, Default)]
pub struct ElementsOptions {
    /// Returned opaque parent ID; None requests the projected root.
    pub root_id: Option<String>,
    /// Returned continuation token, never a URL to follow.
    pub continuation_token: Option<String>,
    /// Source clamps larger sizes to 1000; this API accepts 1..=1000 explicitly.
    pub page_size: Option<u16>,
    /// Explicit cache policy, supported only on private single-level reads.
    pub response_source: Option<ResponseSource>,
}

impl ElementsOptions {
    /// Rejects invalid options before request generation.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(value) = &self.root_id {
            selector(value, 8192)?;
        }
        if let Some(value) = &self.continuation_token {
            selector(value, 16_384)?;
        }
        if self.page_size.is_some_and(|size| size == 0 || size > 1000) {
            return Err(ValidationError::InputLimit);
        }
        Ok(())
    }
}
