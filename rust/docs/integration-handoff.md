<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared Rust integration handoff

The owner standardized on `feature/rust` in
[`eisber/fabric-cli`](https://github.com/eisber/fabric-cli/tree/feature/rust)
on October 23, 2026. Only the integration owner pushes the shared branch.
Lane owners continue on separate branches and adopt an exact validated checkpoint.
Upstream `microsoft/fabric-cli` is not the push destination. The former scaffold
branch remains intact; references to it in dated evidence are historical.

## Captured committed progress

This consolidation starts at scaffold checkpoint
`4d740138ae738ffdca64415638ca491593da7c86` and records these exact heads:

| Lane | Captured head | Integration treatment |
| --- | --- | --- |
| SDK and CLI | `cbb6b6a554854d8a1d10ef9535f225a2e819922c` | Non-fast-forward merge; includes focused workspace SDK and Notebook deployment |
| Native deployment | `79654cf3d08e0276d6aec9b55c59edd1db6d8ec4` | Already an ancestor of the captured SDK head |
| Identity | `ea858c660579d47fb8a47d0a888e279e96dc3be4` | Non-fast-forward merge; optional native boundary reconciled with layer-based crate checks |
| Build and distribution | `997c1447087a987433d69d227a6653b49d7a5d1c` | Non-fast-forward merge; affected consumers, native matrix, resource controls and optional identity checks retained |
| Independent compatibility | `e3366ccccef486579867cd43cd6754c56b8e7439` | Non-fast-forward merge; owner-confirmed gate retained alongside later Windows oracle fixes |
| Plugins and skills | `a43f2b97dcc6ffa63a85d3439f56a8450b536519` | Non-fast-forward ancestry merge; equivalent content already cherry-picked |
| Python unit-test coverage | `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` | Baseline ancestor; no committed implementation delta |

All seven active lane worktrees were clean at capture. Uncommitted peer work is
never copied or included. A later lane head is a new handoff, not implicit
permission to expand this snapshot.

The merge preserves immutable fixtures and source digests, the synthetic Windows
hostname, exactly-once controlled headless fallback, and CRLF-drift rejection.
The production Python source/tests are unchanged. Native Notebook support and
Windows interop remain bounded library slices, not complete deployment or
authentication parity. The identity backend decision remains unresolved.
See [architecture](architecture.md), [deployment compatibility](deployment-compatibility.md)
and [independent validation](validation-matrix.md) for their limits.

## Local consolidation validation

The merged graph was validated on Windows 11 x64, with actual compiler host
`x86_64-pc-windows-msvc`, Rust 1.99.1 (`58cf8f9c3`), Cargo 1.99.1 and LLVM 23.1.1.
Builds used one job, a private target directory, and single-threaded tests:

- Locked metadata, formatting, all-target workspace check and Clippy with
  `-D warnings` passed.
- Offline locked workspace tests passed: 89 non-documentation tests and three
  doctests, with none failed, ignored or filtered out. The explicit documentation
  test command also passed all three doctests.
- Optional broker-feature Clippy and the safe-consumer example with the feature
  both disabled and enabled passed without authentication. The offline injected
  SDK example and CLI help/version passed.
- Independent Python reference validation passed on Python 3.13.15: 13 workspace
  harness tests, all eight standalone cases, and all 44 pinned deployment leaf
  cases (PyYAML 6.0.3, jsonpath-ng 1.9.1). Expectations and source digest were
  unchanged. No full Python suite or coverage repeat was run.
- The actual CI selector passed its embedded assertions and manual/new-branch/
  scaffold-base executions against real merged Cargo metadata, selecting all
  seven members. YAML checks retained the three native runners, timeouts and
  failure gates.

These local results are not Linux/macOS execution evidence. Native provider
results must be read at the exact published head, not inferred from historical
scaffold runs. The coverage percentages and the existing failed headless UI test
in [validation baselines](validation-baselines.md) remain historical evidence,
not an improved or newly green Python baseline.

## Safe synchronization

The integration owner supplies the full validated checkpoint SHA and actual
provider results separately. Do not substitute a presumed latest branch tip.
In **your own lane worktree**, inspect status and checkpoint any authorized
local work first. If unrelated edits are present, preserve them and coordinate
before merging; do not automatically stash or discard them.

For an existing remote named `eisber` pointing to the fork above:

```powershell
git --no-pager status --short --branch
git remote get-url eisber
git fetch eisber
git show --no-patch --format=fuller <agreed-checkpoint>
git merge-base --is-ancestor <agreed-checkpoint> eisber/feature/rust
git merge --no-ff <agreed-checkpoint>
```

Run the merge only after checking that the agreed commit exists on the published
integration branch and that your current branch is your lane branch, not the
shared branch. An ancestor merge is a harmless no-op. Resolve any divergence
semantically in your own lane; do not reset, rebase published work, or force-push
as a synchronization shortcut. New merge commits must retain the repository's
required co-author trailer.

Run Cargo from `rust/`, with a private target directory and the shared-host
validation grant. Use targeted tests first; the full integration commands remain
in [architecture](architecture.md#build-and-contribution-loop).
Do not infer cross-platform success from local Windows results, CI configuration,
or historical scaffold runs. No PR, release, upstream push, live login or service
operation is part of this branch handoff.
