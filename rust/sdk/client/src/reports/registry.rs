// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::v2::{
    Availability, Binding, ContractError, Effect, Generation, OperationDescriptor, Prerequisite,
    Registry, Verb,
};
use serde_json::json;

/// Add only Report metadata and native-source read bindings to a shared registry.
///
/// Facade `report` constrains these same operations; it adds no duplicate handler
/// or tool. Registration performs no authorization, discovery or execution.
/// Related-model navigation still needs its actual owner and separate permission.
///
/// # Errors
/// Duplicate/conflicting entries fail without partially modifying the registry.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    let mut next = registry.clone();
    next.alias("report", "Report")?;
    for (id, facet, representations, availability) in [
        (
            "report.get.metadata",
            "item",
            vec!["json"],
            Availability::Available,
        ),
        (
            "report.get.definition",
            "definition",
            vec!["native", "PBIR", "PBIR-Legacy"],
            Availability::Conditional(vec![
                Prerequisite {
                    id: "report-read-write".into(),
                    description: "Native definition retrieval requires Report read/write permissions and the corresponding delegated scope.".into(),
                },
                Prerequisite {
                    id: "encrypted-label-usage-rights".into(),
                    description: "An encrypted sensitivity label can additionally restrict definition retrieval; labels are not included in native parts.".into(),
                },
            ]),
        ),
    ] {
        next.register(OperationDescriptor::new(
            id,
            Verb::Get,
            json!({"type":"object","properties":{},"additionalProperties":false}),
            json!({"type":"object","description":if facet == "item" {
                "Report metadata only, without related-model reads."
            } else {
                "Exact native definition envelope with separately retained format, version, model-reference prerequisite and incomplete-coverage evidence; not PBIX or served docs."
            }}),
            vec![Effect::RemoteRead],
        )?)?;
        for representation in representations {
            next.bind(Binding::new(
                "Report",
                facet,
                representation,
                Generation::Any,
                id,
                availability.clone(),
            )?)?;
        }
    }
    *registry = next;
    Ok(())
}
