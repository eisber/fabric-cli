<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Deployment reference fixtures

All JSON fixtures in this directory are synthetic Microsoft-authored data under
the repository MIT license. Identifiers, item names, paths, and workspace names
are invented, not captured from a tenant. No credentials, live service calls, or
tenant snapshots are used. Unicode examples are encoded as JSON escapes.

These are **reference leaf contracts, not a native deployment implementation or
end-to-end deployment parity**. See
[deployment compatibility](../../../docs/deployment-compatibility.md) for the
source inventory, boundary exclusions, and proposed next slice.

## Provenance and format

The reference repository is `microsoft/fabric-cicd`, exactly commit
`5ca096c00a9a878c646c59fe537b883eba951329` (MIT licensed). No upstream source files
are vendored here. Each JSON document carries that provenance in `source`; each
case has a unique `id`, leaf `operation`, input, and either a fixed `expected`
value or an error class plus stable diagnostic substring. Error locations,
tracebacks, and parser-version-specific surrounding text are not normalized into
full diagnostic snapshots.

| File | Reference entry points | Cases |
| --- | --- | --- |
| `yaml.json` | PyYAML `safe_load`; `_parameter._parameter._DuplicateKeyLoader` and its constructor/traversal | 8 |
| `config.json` | `_common._config_utils.get_config_value`, `extract_workspace_settings`; `_parameter._utils.process_environment_key` | 5 |
| `ids.json` | `_parameter._utils` dynamic parsing/resolution; `FabricWorkspace._replace_logical_ids`, `_replace_workspace_ids` | 13 |
| `transforms.json` | `_parameter._utils.replace_key_value`; `FabricWorkspace._replace_parameters` and its find/filter helpers | 18 |

Strings in `expected` are exact, including whitespace, escaping, key order, and
line endings. Non-string JSON results are structurally compared with types
preserved (`true` is not `1`). `yaml_types` records Python scalar type names and
string representations, so dates and non-finite floats are explicit rather than
being silently coerced into JSON. An optional `calls` field asserts synthetic
workspace callback order.

## Development oracle

`reference_oracle.py` is a development-only Python oracle. It has no generation,
update, skip, network, credential, or native-runtime bridge mode. It checks fixed
expectations and exits nonzero on a mismatch; it does not rewrite fixtures.

Supply a separately acquired local checkout of the public pinned reference.
The script verifies its exact HEAD and unchanged selected source files, then
compiles selected AST definitions and assignments **without modifying their
bodies**. It avoids package initialization, workspace construction, ambient
environment evaluation, logging setup, and auth/service imports. The sole
function-local import in `_replace_parameters` is routed to the same extracted
parameter utilities. The duplicate-loader constructor is registered as in the
upstream module. This is not a general-purpose untrusted-code sandbox.

Synthetic workspace objects expose fixed data, a counted no-I/O deployed-item
refresh callback, and a fixed workspace-name callback. They do not test real
metadata retrieval, caching, synchronization, credentials, or missing-service
responses. Named/cross-workspace cases test parsing only, not resolution.
Filesystem filtering is deliberately rejected by this oracle; item type/name
filtering uses the actual reference helper. YAML parsing-only cases call the
same PyYAML loader as the corresponding source boundary, not the full config or
parameter validators.

Use Python 3.11 or later with **PyYAML 6.0.3** and **jsonpath-ng 1.9.1**.
The latter matches the pinned upstream Python >=3.11 dependency range; the
upstream range is not itself an exact dependency lock. Python 3.10's upstream
jsonpath-ng 1.7.0 branch is not covered. Dependency drift fails explicitly.
Install nothing globally; dependency restoration and execution on a shared host
require the coordinator's bounded grant.

From the repository root on Windows, after that grant:

```powershell
python rust\tests\fixtures\deploy\reference_oracle.py --reference-root C:\path\to\fabric-cicd-reference
```

The successful JSON report records actual Python/platform/dependency versions,
case count, and SHA-256 hashes of the source and fixture files. Keep the report
as a session artifact, not a regenerated compatibility baseline. A passing
reference run proves the sampled Python leaf behavior only; a future independent
native adapter must consume the same fixtures without changing their oracle.

Integration validation executed all **44 cases successfully** against the pinned
reference on Windows x64, Python 3.13.15, PyYAML 6.0.3, and jsonpath-ng 1.9.1.
See [validation baselines](../../../docs/validation-baselines.md). This validates
the selected Python leaf fixtures only, not a native adapter or other platforms.
