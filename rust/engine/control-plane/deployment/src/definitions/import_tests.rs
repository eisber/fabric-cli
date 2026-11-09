// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::current_dir().unwrap().join(format!(
            ".import-source-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn existing_regular_file_matches_actual_python_walk_empty_parts_not_file_contents() {
    let root = Root::new();
    let path = root.0.join("file.bin");
    fs::write(&path, b"not traversed by os.walk").unwrap();
    let limits = DefinitionLimits::default();
    let mut files = LocalDefinitionFileSystem::new(limits);
    let loaded = load_import_definition(&mut files, &path, None, limits).unwrap();
    assert_eq!(loaded.definition, json!({"parts":[]}));
    assert!(loaded.excluded_paths.is_empty());
}

#[test]
fn environment_preparation_preserves_generic_raw_artifact_order_without_notebook_rewrites() {
    let root = Root::new();
    fs::create_dir(root.0.join("Resources")).unwrap();
    fs::write(
        root.0.join("environment.yml"),
        b"dependencies:\r\n  - numpy\n",
    )
    .unwrap();
    fs::write(root.0.join("Resources").join("b.whl"), [0, 255, 13, 10]).unwrap();
    fs::write(root.0.join("Resources").join("a.jar"), b"jar").unwrap();
    fs::write(root.0.join("cache.abf"), b"excluded").unwrap();
    let limits = DefinitionLimits::default();
    let mut files = LocalDefinitionFileSystem::new(limits);
    let target = ImportTarget {
        workspace_id: "11111111-1111-1111-1111-111111111111",
        item_id: None,
        item_type: "Environment",
        display_name: "Synthetic",
        folder_id: None,
    };
    let prepared =
        PreparedImport::prepare_environment(&mut files, &root.0, target, None, true, limits)
            .unwrap();
    let source: Value = serde_json::from_str(include_str!(
        "../../tests/definition_import_lifecycle_source.json"
    ))
    .unwrap();
    assert_eq!(
        prepared.body(),
        source["cases"][0]["requests"][0]["body"]
            .as_str()
            .unwrap()
            .as_bytes()
    );
    assert_eq!(prepared.excluded_paths, ["cache.abf"]);
}

#[test]
fn environment_opt_in_does_not_weaken_ids_confirmation_format_or_source_type_checks() {
    let root = Root::new();
    let limits = DefinitionLimits::default();
    let mut files = LocalDefinitionFileSystem::new(limits);
    let target = ImportTarget {
        workspace_id: "11111111-1111-1111-1111-111111111111",
        item_id: None,
        item_type: "Environment",
        display_name: "Synthetic",
        folder_id: None,
    };
    assert!(matches!(
        PreparedImport::prepare_environment(&mut files, &root.0, target, None, false, limits)
            .unwrap_err()
            .kind,
        Kind::ConfirmationRequired
    ));
    for (target, selector) in [
        (
            ImportTarget {
                item_type: "Notebook",
                ..target
            },
            None,
        ),
        (
            ImportTarget {
                folder_id: Some("../folder"),
                ..target
            },
            None,
        ),
        (target, Some(".ipynb")),
    ] {
        assert!(matches!(
            PreparedImport::prepare_environment(
                &mut files, &root.0, target, selector, true, limits
            )
            .unwrap_err()
            .kind,
            Kind::InvalidInput
        ));
    }
}
