// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Builds the managed component only when explicitly selected.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_DOTNET");
    if env::var_os("CARGO_FEATURE_DOTNET").is_none() {
        return;
    }

    println!("cargo:rerun-if-changed=managed");
    println!("cargo:rerun-if-env-changed=FABRIC_BPA_DOTNET_BUILD_HOST");
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let workspace = workspace_root(&manifest);
    let config = workspace.join("NuGet.Config");
    let lock = manifest.join("managed").join("dependencies.lock.json");
    println!("cargo:rerun-if-changed={}", config.display());
    println!("cargo:rerun-if-changed={}", lock.display());
    assert!(
        config.is_file(),
        "dotnet feature requires the workspace-owned public-only rust/NuGet.Config"
    );
    assert!(
        lock.is_file(),
        "dotnet feature requires reviewed managed/dependencies.lock.json; bootstrap it with an explicitly authorized isolated restore"
    );
    let component = output.join("managed");
    for directory in [
        "managed",
        "obj",
        "bin",
        "packages",
        "dotnet-home",
        "dotnet-home/AppData/Roaming",
        "dotnet-home/AppData/Local",
        "scratch",
        "http-cache",
        "plugin-cache",
    ] {
        fs::create_dir_all(output.join(directory))
            .expect("create isolated managed build directory");
    }
    let executable = env::var_os("FABRIC_BPA_DOTNET_BUILD_HOST").unwrap_or_else(|| "dotnet".into());
    let mut command = managed_publish_command(&manifest, &output, executable);
    let status = command
        .args([
            "--configuration",
            "Release",
            "--nologo",
            "--verbosity",
            "quiet",
        ])
        .arg("--output")
        .arg(&component)
        .args(["--self-contained", "false"])
        .arg(format!(
            "-p:BaseIntermediateOutputPath={}{}",
            output.join("obj").display(),
            std::path::MAIN_SEPARATOR
        ))
        .arg(format!(
            "-p:RestorePackagesPath={}",
            output.join("packages").display()
        ))
        .arg(format!(
            "-p:BaseOutputPath={}{}",
            output.join("bin").display(),
            std::path::MAIN_SEPARATOR
        ))
        .arg(format!("-p:RestoreConfigFile={}", config.display()))
        .args([
            "-p:RestoreLockedMode=true",
            "-p:RestorePackagesWithLockFile=true",
            "-p:NuGetInteractive=false",
            "-nodeReuse:false",
        ])
        .output()
        .expect("dotnet feature requires a .NET 8 SDK; no subprocess is used at analysis time");
    if !status.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&status.stdout));
        eprintln!("{}", String::from_utf8_lossy(&status.stderr));
        panic!(
            "managed TOM component build failed; validate the managed project with the documented isolated restore/build procedure"
        );
    }
    println!(
        "cargo:rustc-env=FABRIC_BPA_COMPONENT_DIR={}",
        component.display()
    );
}

pub(crate) fn workspace_root(manifest: &Path) -> &Path {
    manifest
        .ancestors()
        .find(|directory| directory.join("rust-toolchain.toml").is_file())
        .expect("workspace layout")
}

pub(crate) fn managed_publish_command(
    manifest: &Path,
    output: &Path,
    executable: impl AsRef<std::ffi::OsStr>,
) -> Command {
    let working = output.join("build-root");
    fs::create_dir_all(&working).expect("create isolated managed build working directory");
    let sdk_pin = fs::read(manifest.join("managed/global.json")).expect("read pinned managed SDK");
    fs::write(working.join("global.json"), &sdk_pin).expect("copy pinned managed SDK selection");
    let mut command = Command::new(executable);
    isolate_build_environment(&mut command, output);
    command
        .current_dir(working)
        .arg("publish")
        .arg(manifest.join("managed/Fabric.SemanticModel.Managed.csproj"));
    command
}

fn isolate_build_environment(command: &mut Command, output: &Path) {
    // Do not forward source credentials, credential providers, MSBuild restore
    // overrides or arbitrary SDK startup hooks into the build-only subprocess.
    command.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS",
        "DOTNET_ROOT",
        "DOTNET_ROOT_X64",
        "DOTNET_ROOT_ARM64",
    ] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    let home = output.join("dotnet-home");
    let appdata = home.join("AppData/Roaming");
    let localappdata = home.join("AppData/Local");
    fs::create_dir_all(&appdata).expect("create isolated profile roaming data directory");
    fs::create_dir_all(&localappdata).expect("create isolated profile local data directory");
    command
        .env("DOTNET_CLI_HOME", &home)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("APPDATA", appdata)
        .env("LOCALAPPDATA", localappdata)
        .env("NUGET_PACKAGES", output.join("packages"))
        .env("NUGET_HTTP_CACHE_PATH", output.join("http-cache"))
        .env("NUGET_PLUGINS_CACHE_PATH", output.join("plugin-cache"))
        .env("NUGET_SCRATCH", output.join("scratch"))
        .env("TEMP", output.join("scratch"))
        .env("TMP", output.join("scratch"))
        .env("TMPDIR", output.join("scratch"))
        // NuGet 6.x (pinned .NET 8 SDK) treats a nonempty ";" as an explicit
        // empty plugin list rather than falling back to user plugin discovery.
        .env("NUGET_NETCORE_PLUGIN_PATHS", ";")
        .env("NUGET_PLUGIN_PATHS", ";")
        .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_GENERATE_ASPNET_CERTIFICATE", "false")
        .env("DOTNET_ADD_GLOBAL_TOOLS_TO_PATH", "false")
        .env("DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE", "true")
        .env("MSBUILDDISABLENODEREUSE", "1");
}
