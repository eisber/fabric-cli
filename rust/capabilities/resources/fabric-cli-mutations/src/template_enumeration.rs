// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-owned, ordered template walk inputs; no ambient filesystem access.

use crate::MutationError;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One top-down `os.walk`-shaped listing, using template-relative slash paths.
#[derive(Clone, Debug)]
pub struct TemplateDirectory {
    /// Relative directory; the template root is the empty string.
    pub directory: String,
    /// Immediate child directory names, in traversal order.
    pub directories: Vec<String>,
    /// Immediate file names, in enumeration order.
    pub files: Vec<String>,
}

/// Explicit walk traces keyed by pinned template directory name.
///
/// Every consumed template must have a complete, unique, top-down trace.
/// Unknown, missing, duplicate or invalid paths fail with
/// `InvalidTemplateEnumeration`, an input safety classification rather than a
/// claim about Python's permissive synthetic/duplicate walk behavior.
#[derive(Clone, Debug, Default)]
pub struct TemplateEnumeration {
    /// For example, `Blank.Report` and `Blank.SemanticModel` for dependent creation.
    pub templates: BTreeMap<String, Vec<TemplateDirectory>>,
}

impl TemplateEnumeration {
    pub(crate) fn apply(
        &self,
        template: &str,
        mut definition: Value,
    ) -> Result<Value, MutationError> {
        let rows = self.templates.get(template).ok_or_else(invalid)?;
        let parts = definition["parts"].as_array().expect("embedded parts");
        let mut inventory: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> =
            BTreeMap::from([(String::new(), (BTreeSet::new(), BTreeSet::new()))]);
        for part in parts {
            let path = part["path"].as_str().expect("embedded path");
            let mut directory = String::new();
            let mut segments = path.split('/').peekable();
            while let Some(segment) = segments.next() {
                if segments.peek().is_some() {
                    inventory
                        .entry(directory.clone())
                        .or_default()
                        .0
                        .insert(segment.into());
                    directory = join(&directory, segment);
                    inventory.entry(directory.clone()).or_default();
                } else {
                    inventory
                        .entry(directory.clone())
                        .or_default()
                        .1
                        .insert(segment.into());
                }
            }
        }
        let mut pending = vec![String::new()];
        let mut ordered = Vec::with_capacity(parts.len());
        for row in rows {
            if pending.pop().as_deref() != Some(row.directory.as_str()) {
                return Err(invalid());
            }
            let (directories, files) = inventory.get(&row.directory).ok_or_else(invalid)?;
            if !complete_names(&row.directories, directories) || !complete_names(&row.files, files)
            {
                return Err(invalid());
            }
            for name in &row.files {
                let path = join(&row.directory, name);
                ordered.push(
                    parts
                        .iter()
                        .find(|part| part["path"] == path)
                        .expect("validated embedded file")
                        .clone(),
                );
            }
            pending.extend(
                row.directories
                    .iter()
                    .rev()
                    .map(|name| join(&row.directory, name)),
            );
        }
        if !pending.is_empty() || ordered.len() != parts.len() {
            return Err(invalid());
        }
        definition["parts"] = ordered.into();
        Ok(definition)
    }
}

fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.into()
    } else {
        format!("{directory}/{name}")
    }
}

fn complete_names(names: &[String], expected: &BTreeSet<String>) -> bool {
    names.len() == expected.len()
        && names.iter().all(|name| {
            !name.is_empty()
                && !matches!(name.as_str(), "." | "..")
                && !name.contains(['/', '\\', ':', '\0'])
                && expected.contains(name)
        })
        && names.iter().collect::<BTreeSet<_>>().len() == names.len()
}

fn invalid() -> MutationError {
    MutationError::new(
        "InvalidTemplateEnumeration",
        "Template enumeration must be a complete unique top-down listing of the pinned template",
    )
}

pub(crate) fn apply(
    enumeration: Option<&TemplateEnumeration>,
    template: &str,
    definition: Value,
) -> Result<Value, MutationError> {
    match enumeration {
        Some(enumeration) => enumeration.apply(template, definition),
        None => Ok(definition),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enumeration() -> TemplateEnumeration {
        TemplateEnumeration {
            templates: BTreeMap::from([(
                "Blank.SemanticModel".into(),
                vec![
                    TemplateDirectory {
                        directory: String::new(),
                        directories: vec!["definition".into()],
                        files: vec!["definition.pbism".into(), ".platform".into()],
                    },
                    TemplateDirectory {
                        directory: "definition".into(),
                        directories: vec!["tables".into()],
                        files: vec!["model.tmdl".into(), "database.tmdl".into()],
                    },
                    TemplateDirectory {
                        directory: "definition/tables".into(),
                        directories: vec![],
                        files: vec!["Table.tmdl".into()],
                    },
                ],
            )]),
        }
    }

    #[test]
    fn supplied_order_changes_only_part_positions() {
        let bundled = crate::templates::semantic_model(None).unwrap();
        let reordered = crate::templates::semantic_model(Some(&enumeration())).unwrap();
        let parts = reordered["parts"].as_array().unwrap();
        assert_eq!(
            parts
                .iter()
                .map(|part| part["path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "definition.pbism",
                ".platform",
                "definition/model.tmdl",
                "definition/database.tmdl",
                "definition/tables/Table.tmdl",
            ]
        );
        for part in parts {
            assert!(
                bundled["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|original| original == part)
            );
        }
        assert_eq!(bundled["parts"][0]["path"], ".platform");
        assert_eq!(bundled["parts"][2]["path"], "definition/database.tmdl");
    }

    #[test]
    fn missing_duplicate_invalid_and_non_top_down_inputs_fail_closed() {
        let definition = crate::templates::semantic_model(None).unwrap();
        assert_eq!(
            TemplateEnumeration::default()
                .apply("Blank.SemanticModel", definition.clone())
                .unwrap_err()
                .code,
            "InvalidTemplateEnumeration"
        );
        for variant in 0..7 {
            let mut input = enumeration();
            let rows = input.templates.get_mut("Blank.SemanticModel").unwrap();
            match variant {
                0 => {
                    rows[0].files.pop().unwrap();
                }
                1 => rows[0].files[1] = rows[0].files[0].clone(),
                2 => rows[0].files[0] = "../definition.pbism".into(),
                3 => rows.swap(1, 2),
                4 => rows[0].directories.clear(),
                5 => rows.push(rows[2].clone()),
                _ => rows[1].directory = "definition\\tables".into(),
            }
            assert_eq!(
                input
                    .apply("Blank.SemanticModel", definition.clone())
                    .unwrap_err()
                    .code,
                "InvalidTemplateEnumeration"
            );
        }
    }

    #[test]
    fn single_part_mirror_bytes_and_default_api_are_preserved() {
        let raw = crate::templates::generic_mirror(None).unwrap();
        let input = TemplateEnumeration {
            templates: BTreeMap::from([(
                "MirroredDatabase.GenericMirror".into(),
                vec![TemplateDirectory {
                    directory: String::new(),
                    directories: vec![],
                    files: vec!["mirroring.json".into()],
                }],
            )]),
        };
        assert_eq!(crate::templates::generic_mirror(Some(&input)).unwrap(), raw);
        assert_eq!(apply(None, "unused", raw.clone()).unwrap(), raw);
    }
}
