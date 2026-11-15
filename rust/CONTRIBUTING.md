<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Contributing to the Rust CLI

This guide covers local development of `fab-rs`, the Rust executable built by
the `fabric-cli` Cargo package. It runs alongside Python `fab`; the Python
development environment and debugger configuration do not build or debug it.
Follow the [root contribution process](../CONTRIBUTING.md) for issue approval,
pull requests, change entries, and the code of conduct.

## Set up your environment

1. Check out a revision that contains the Rust workspace, then run commands
   from `rust/`:

   ```bash
   # From the repository root.
   cd rust
   ```

2. Install Rust with [rustup](https://rustup.rs/) and your platform's build tools:
   - On macOS, install Xcode Command Line Tools, which include the linker and LLDB.
   - On Linux, install a C compiler and linker through your distribution's build
     tools package. Install LLDB separately for the debugging steps.
   - On Windows, install Visual Studio Build Tools with **Desktop development
     with C++** and a Windows SDK for the MSVC target. Install Visual Studio
     with that workload if you want to use its debugger.

3. Install the toolchain pinned in `rust-toolchain.toml`:

   ```bash
   rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
   rustc --version
   cargo --version
   ```

   Rustup selects this toolchain inside `rust/`. Do not substitute `stable`
   or change the pin to work around an installation failure.

4. Restore the locked dependencies:

   ```bash
   cargo fetch --locked
   ```

   This step requires registry access. Subsequent commands use `--offline`
   and `--locked` to use the restored dependencies without updating `Cargo.lock`.
   Development scripts require Python 3.11 or later; use `python` instead of
   `python3` on Windows if needed. Python is not required to run `fab-rs`.

**On a shared host, obtain the integration owner's explicit heavy-build lease
before any Cargo build, run, check, test, clippy, or doc command. Run only one
heavy job at a time.** Formatting and metadata inspection do not require a lease.

## Build and run locally

Build the development binary from `rust/`:

```bash
cargo build --offline --locked -p fabric-cli --bin fab-rs
cargo run --offline --locked -p fabric-cli --bin fab-rs -- --help
cargo run --offline --locked -p fabric-cli --bin fab-rs -- api --help
```

The default development profile includes debug information and disables
optimizations. Cargo arguments precede `--`; arguments after it go to `fab-rs`.
Help, version, and subcommand help do not require Azure CLI or authentication,
so they are suitable for an unauthenticated smoke check.

You can also run the built executable without Cargo:

```bash
# macOS or Linux, from rust/.
./target/debug/fab-rs --help
./target/debug/fab-rs --version
```

```powershell
# Windows PowerShell, from rust/.
.\target\debug\fab-rs.exe --help
.\target\debug\fab-rs.exe --version
```

Build an optimized binary with the release profile:

```bash
cargo build --offline --locked --release -p fabric-cli --bin fab-rs
```

The release executable is `target/release/fab-rs`, or
`target/release/fab-rs.exe` on Windows. The default release profile omits debug
information; use the development build for source-level debugging.
These paths assume no target-directory override and no explicit `--target`.
An explicit target places binaries under `target/<target>/debug/` or
`target/<target>/release/`; Cargo metadata reports the actual target directory.

Only authentication commands and raw `api` are implemented. For authenticated
use, read the [run instructions](README.md#run) and
[compatibility contract](docs/compatibility.md). Azure CLI 2.54 or later is the
sole credential source. Rust authentication binds to an existing Azure CLI
identity; it does not sign you in to Azure or change Azure CLI state.

## Debug locally

### macOS and Linux with LLDB

Install LLDB separately if it is not available on your PATH. `rust-lldb` is the
Rust toolchain wrapper that loads Rust debugger helpers; it does not install
LLDB. Build the development binary first, then launch from `rust/`:

```bash
rust-lldb target/debug/fab-rs -- --help
```

At the LLDB prompt, set a source breakpoint in
`clients/cli/app/src/main.rs` before running the short-lived help command:

```text
(lldb) breakpoint set --file main.rs --source-pattern-regexp 'fn main'
(lldb) run
(lldb) next
(lldb) frame variable
(lldb) continue
(lldb) quit
```

Use `step` instead of `next` to enter a function. Help returns before
authentication and runtime setup; use the offline tests to investigate behavior
that requires credentials or HTTP. Do not use live Azure sessions for tests.

### Windows with Visual Studio

Build the development binary for MSVC. Keep the generated `.pdb` symbol file
alongside `target/debug/fab-rs.exe` so the debugger can load its symbols.
From a Visual Studio Developer PowerShell in `rust/`, open the executable:

```powershell
devenv /debugexe .\target\debug\fab-rs.exe "--help"
```

Open `clients/cli/app/src/main.rs`, set a breakpoint inside `main`, and select
**Debug > Start Debugging**. Use **Step Over** to advance through source lines
and **Locals** to inspect values. Rust-specific type display differs between
debuggers; see the [Visual Studio executable debugging instructions](https://learn.microsoft.com/en-us/visualstudio/debugger/how-to-debug-an-executable-not-part-of-a-visual-studio-solution?view=vs-2022)
and [`/DebugExe` reference](https://learn.microsoft.com/en-us/visualstudio/ide/reference/debugexe-devenv-exe?view=vs-2022).

The repository's `.vscode/launch.json` targets Python, not `fab-rs`.
It is not a Rust debugger setup.

## Validate and package your changes

For a focused CLI change, run these commands from `rust/` after obtaining the
shared-host lease:

```bash
cargo fmt --all -- --check
cargo test --offline --locked -p fabric-cli
```

Use the [workspace validation commands](README.md#development-and-validation)
before submitting changes that affect multiple packages. Tests use injected
credentials and HTTP responses, not ambient Azure CLI or live services.
See the [CLI composition guide](clients/cli/README.md) for entry points and tests.

A release binary is not a distributable bundle. Follow the
[packaging instructions](docs/packaging.md) to record host evidence, build with
an explicit matching target, package the binary, and verify the archive.
Packaging requires a clean checkout, restored dependencies, Python 3.11 or
later, and platform inspection tools (`readelf` on Linux or `otool` on macOS).
The packaging script does not compile, sign, or publish.

Build and verify each bundle on its matching OS and architecture; selecting
another `--target` does not make cross-platform packaging or testing work
automatically. Preview bundles are unsigned and unofficial. Read the
[validation evidence](docs/validation.md) for the supported build targets and
the limits of current OS compatibility evidence.
