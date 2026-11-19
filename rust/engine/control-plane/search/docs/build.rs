// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Verify a finalized index bundle and generate its immutable byte inventory.

use std::{collections::BTreeSet, env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FAB_DOCS_INDEX");
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR")).join("embedded.rs");
    let Some(root) = env::var_os("FAB_DOCS_INDEX").map(PathBuf::from) else {
        fs::write(
            output,
            "pub(crate) const MANIFEST: Option<&str> = None;\npub(crate) const FILES: &[(&str, &[u8])] = &[];\n",
        )
        .expect("write engine-only embedding descriptor");
        return;
    };
    let root = root.canonicalize().expect("FAB_DOCS_INDEX must exist");
    let manifest_path = root.join("bundle.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let manifest_bytes = fs::read(&manifest_path).expect("read index bundle manifest");
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).expect("valid index bundle manifest JSON");
    assert_eq!(manifest["schema_version"], 1, "unsupported bundle schema");
    assert_eq!(
        manifest["tantivy_version"], "0.27.0",
        "index version mismatch"
    );
    assert_eq!(
        manifest["schema_id"], "fabric-docs-sections-v2",
        "index field/analyzer contract mismatch; rebuild the documentation bundle"
    );
    let files = manifest["files"]
        .as_array()
        .expect("manifest file inventory");
    assert!(!files.is_empty(), "index bundle must not be empty");
    let mut names = BTreeSet::new();
    let mut generated = format!(
        "pub(crate) const MANIFEST: Option<&str> = Some(include_str!({:?}));\npub(crate) const FILES: &[(&str, &[u8])] = &[\n",
        manifest_path.to_str().expect("UTF-8 manifest path")
    );
    for file in files {
        let name = file["name"].as_str().expect("component filename");
        assert!(
            !name.is_empty()
                && !name.contains(['/', '\\', ':'])
                && name != "."
                && name != ".."
                && names.insert(name),
            "invalid or duplicate index component"
        );
        let path = root.join(name);
        assert!(
            !fs::symlink_metadata(&path)
                .expect("component metadata")
                .is_symlink()
        );
        let bytes = fs::read(&path).expect("read index component");
        assert_eq!(file["bytes"].as_u64(), Some(bytes.len() as u64));
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let digest = digest
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(
            file["sha256"].as_str(),
            Some(digest.as_str()),
            "component checksum mismatch"
        );
        println!("cargo:rerun-if-changed={}", path.display());
        generated.push_str(&format!(
            "({name:?}, include_bytes!({:?})),\n",
            path.to_str().expect("UTF-8 component path")
        ));
    }
    assert!(names.contains("meta.json"), "bundle has no index metadata");
    generated.push_str("];\n");
    fs::write(output, generated).expect("write immutable embedding descriptor");
}
