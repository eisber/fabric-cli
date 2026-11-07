// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline source-write isolation for the optional managed build.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

#[test]
fn managed_build_uses_output_working_directory_and_exact_original_sdk_pin() {
    let root = std::env::var_os("FAB_IDENTITY_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let unique = format!(
        "semantic-build-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let fixture = root.join(unique);
    let source = fixture.join("source");
    let output = fixture.join("artifacts");
    std::fs::create_dir_all(source.join("managed")).unwrap();
    let original = br#"{"sdk":{"version":"8.0.425","rollForward":"latestPatch"}}"#;
    std::fs::write(source.join("managed/global.json"), original).unwrap();
    let command = build_script::managed_publish_command(&source, &output, "dotnet");
    assert_eq!(
        command.get_current_dir(),
        Some(output.join("build-root").as_path())
    );
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            std::ffi::OsStr::new("publish"),
            source
                .join("managed/Fabric.SemanticModel.Managed.csproj")
                .as_os_str(),
        ]
    );
    assert_eq!(
        std::fs::read(output.join("build-root/global.json")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read(source.join("managed/global.json")).unwrap(),
        original
    );
    assert!(!source.join("managed/NuGet").exists());
    std::fs::remove_dir_all(fixture).unwrap();
}
