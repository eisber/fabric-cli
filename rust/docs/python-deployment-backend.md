<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Python-backed deployment in Rust v1

On **November 2, 2026**, the owner explicitly approved that deployment can invoke
Python: a **deployment-only** exception superseding earlier entire-Rust/no-Python
deployment restrictions. Executable
`fab-rs deploy` always selects the installed Python Fabric CLI before native
configuration, credential acquisition, repository preparation or service effects.
There is no native-first attempt, automatic fallback, retry or shadow deployment.
The experimental native deployment libraries and their offline contracts remain
available to explicit library callers; they are not the executable backend.

This delegates deployment configuration, item support, Python regular expressions
and extended JSONPath to their authoritative implementation. Rust's bounded
regex/JSONPath implementations therefore do not reject otherwise supported
executable deployments. Support still depends on the selected Python CLI and
`fabric-cicd` versions; this is not a new independent deployment engine or proof
that every source configuration works.

## Runtime and package contract

Install Python **3.10 through 3.14** and the supported **`ms-fabric-cli`** package
with its normal dependencies in that environment before invoking deployment.
The checked-in product declares `fabric-cicd>=1.4.0` and `psutil==7.0.0` among
those dependencies. Provisioning is an installation/operator step, not a Rust
runtime operation: the adapter never downloads code, installs packages or
repairs an environment.

Selection is exact and occurs once per command:

1. If `FAB_RUST_DEPLOY_PYTHON` is set, use its value as **one executable name or
   path**, including spaces. An empty value is invalid. Do not include surrounding
   command-line quotes, interpreter flags, shell syntax or credentials.
2. Otherwise use `python` on Windows and `python3` on other platforms, resolved
   by the operating system's executable search. No alternative runtime is tried.

No existing product Python-executable override was found. This deployment-specific
variable does not select authentication or another command's backend.

Tradeoff: default `fab-rs` now includes the existing Tokio runtime for deployment signal handling, without adding a broker or transport dependency edge.

The invocation is equivalent to:

```text
<selected-python> -I -B -u -c <fixed-adapter> deploy <individual-argument-values>
```

`-I` ignores `PYTHONPATH`, user-site packages and current-directory module
shadowing. An installed package, not an uninstalled source checkout, is required.
`-B` avoids bytecode writes and `-u` keeps output streaming. The adapter verifies
package availability and imports the actual console entry point
`fabric_cli.main:main`, declared by `pyproject.toml`; it never invokes a `fab`
executable that might resolve back to Rust. The Python deploy handler delegates
to `fabric_cicd.deploy_with_config` through its existing CLI wrapper, including
that wrapper's parameter parsing, logging and token-credential bridge.

Python deployment uses the Python CLI's existing configuration and authentication
contract. It does **not** consume Rust broker selectors, silently switch an
existing Rust credential to another provider, or share Rust tokens. Operators
must provision the intended Python deployment identity separately. This task's
offline tests do not authorize login, registration changes or service mutations.
Other Rust commands and all SDK dependency edges remain native and unchanged;
there is no Python token-acquisition fallback for them.

## Arguments, outcomes and cleanup

The Rust schema retains config path, target environment, each parameter token,
force, experimental bulk-publish and output-format selections. The adapter passes
these as individual OS arguments without shell interpolation, token joining or
parameter interpretation. Rust parser/help diagnostics remain Rust-owned.
Unknown future Python flags require a corresponding front-door schema update.
Confirmation and deployment behavior otherwise belong to Python.

Standard input, standard output and standard error are inherited directly.
The adapter preserves Python's exit status, including nonzero failures; it emits
no additional success envelope. Missing runtime/package and start/wait errors
are actionable, sanitized errors. The configured executable and argument values
are not included in Rust diagnostics.

On Unix, deployment stays in the parent's foreground process group so non-forced
confirmation can read the inherited controlling terminal without stopping on
`SIGTTIN`. Before loading the deployment CLI, the Python entry ignores exactly
the catchable Unix signals handled by Rust: `SIGINT` and `SIGTERM`. The Rust
parent handles terminal Ctrl-C and SIGTERM cancellation, keeping the Python root
alive for process-tree discovery even when the signal targets the foreground
group. Other signals are not silently masked. Windows retains its separate
process-group creation flag.
On cancellation or a dropped execution future, a bounded
helper in the same interpreter uses the already-required `psutil` package to
terminate and then kill the deployment process tree, including a root that
ignores SIGTERM. Rust reaps its direct child while the helper verifies cleanup;
it also kills and reaps that child after the helper completes.
Normal Python exit cleans up remaining command-owned children.
Failed verification is reported as uncertain cleanup, never success.
Unix SIGTERM is handled as cancellation; a child signal termination is nonzero.
An uncatchable parent termination, such as Unix `SIGKILL`, or machine loss cannot
guarantee cleanup.

Cancellation and local process exit are **not rollback**: previously accepted
service operations may still finish. Inspect the target before retrying an
interrupted deployment. No deployment is replayed automatically.

## Offline validation and acceptance boundary

Adapter tests use synthetic Python modules and child processes, not credentials,
live services or real deployment inputs. They cover spaces/quotes/Unicode,
separate byte streams, exit values, missing packages/runtime, start errors,
pre-cancellation, process-tree cleanup, dropped futures and no replay.
The process tests require an available test Python with `psutil`; optionally set
`FAB_RUST_DEPLOY_TEST_PYTHON` to that environment's executable.
The Unix-only `python_deploy_terminal` integration test uses Python's standard
PTY and venv support, with synthetic installed packages and the existing `psutil`
dependency (no package download). It exercises the actual executable and adapter
for non-forced confirmation, decline, exit preservation, terminal Ctrl-C and
foreground-group SIGTERM cleanup. The group-SIGTERM case includes a descendant
that ignores SIGINT/SIGTERM in a different process group, checks typed cancellation
and verifies that both deployment root and descendant are gone.
That test is excluded on Windows; Windows results do not establish
Linux/macOS terminal behavior.

These process-boundary tests do not establish live deployment parity, credential
policy acceptance, cross-platform installation, signing, release readiness or GA
acceptance. The SDK and native deployment library contracts remain separate.
