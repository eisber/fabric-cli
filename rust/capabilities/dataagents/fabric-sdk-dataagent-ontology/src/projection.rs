// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{models::*, scope::Resource};

pub(crate) trait Projection {
    fn matches(&self, resource: &Resource, selected: Option<&[String]>) -> bool;
}

impl Projection for OntologyAttachment {
    fn matches(&self, resource: &Resource, _: Option<&[String]>) -> bool {
        let requested = match resource {
            Resource::Attachment(scope) => Some(&scope.ontology_id),
            _ => None,
        };
        requested.is_none_or(|id| id == &self.id)
            && self.ontology_reference.as_ref().is_none_or(|reference| reference.item_id == self.id)
    }
}

impl Projection for Element {
    fn matches(&self, _: &Resource, _: Option<&[String]>) -> bool {
        !self.id.is_empty() && !self.kind.is_empty()
    }
}

impl Projection for OntologySource {
    fn matches(&self, _: &Resource, _: Option<&[String]>) -> bool {
        true
    }
}

impl<T: Projection> Projection for Page<T> {
    fn matches(&self, resource: &Resource, selected: Option<&[String]>) -> bool {
        self.value.iter().all(|value| value.matches(resource, selected))
    }
}

impl<T: Projection> Projection for Option<T> {
    fn matches(&self, resource: &Resource, selected: Option<&[String]>) -> bool {
        self.as_ref().is_none_or(|value| value.matches(resource, selected))
    }
}

impl Projection for SourceInstructions {
    fn matches(&self, resource: &Resource, _: Option<&[String]>) -> bool {
        matches!(resource, Resource::Instructions(scope) if scope.source_id == self.artifact_id)
    }
}

impl Projection for SourceSchema {
    fn matches(&self, resource: &Resource, _: Option<&[String]>) -> bool {
        matches!(resource, Resource::Source(scope) if scope.source_id == self.data_source_id)
            && !self.cache_last_updated_time.trim().is_empty()
            && self.elements.iter().all(valid_schema_element)
    }
}

fn valid_schema_element(element: &SchemaElement) -> bool {
    (element.kind.is_string() || element.kind.is_u64())
        && element.children.iter().all(valid_schema_element)
}

impl Projection for FewShot {
    fn matches(&self, resource: &Resource, _: Option<&[String]>) -> bool {
        match resource {
            Resource::FewShot(_, id) => id == &self.id,
            _ => true,
        }
    }
}

impl Projection for SelectionResult {
    fn matches(&self, _: &Resource, selected: Option<&[String]>) -> bool {
        self.elements_status.iter().all(|entry| {
            !entry.element_id.is_empty()
                && matches!(entry.status.as_str(), "Succeeded" | "NotFound" | "Failed")
        }) && selected.is_none_or(|ids| {
            ids.len() == self.elements_status.len()
                && ids.iter().zip(&self.elements_status).all(|(id, status)| id == &status.element_id)
        })
    }
}

impl Projection for PublishResult {
    fn matches(&self, _: &Resource, _: Option<&[String]>) -> bool { true }
}

impl Projection for ContextMarkdown {
    fn matches(&self, _: &Resource, _: Option<&[String]>) -> bool { true }
}

impl Projection for () {
    fn matches(&self, _: &Resource, _: Option<&[String]>) -> bool { true }
}
