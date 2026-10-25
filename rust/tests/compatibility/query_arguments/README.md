<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Read query argument reference

This new oracle measures **raw argv**, not the legacy folder handler's synthetic
`Namespace(query=[...])` protocol. Source is `microsoft/fabric-cli`
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; the native integration base is
`d3d42eca672b387cabc0626901bb3719bab76f74`.

The initial proposed gap was partly incorrect: native GET already accepted
ordinary tokenized queries. Actual Python `register_ls_parser`/`dir` defines a
single scalar query; GET defines `nargs="+"`. A scalar LS query retains edge
quotes; GET tokens join with spaces and strip edge quotes through the existing
`process_nargs`. Split LS query-looking words can instead become path tokens;
they must never be silently joined into the query.

## Independent evidence

`cases.json` contains **60 hand-authored parse cases: 42 accepted / 18 rejected**.
Each is checked against the complete, blob-verified original
`CustomArgumentParser`, `CustomHelpFormatter`, global flag registration, read
registrations and `process_nargs`. The existing full Python-tree source pin is
also checked. No ambient product import, authentication, real HTTP, filesystem
product write, subprocess or sleep is allowed during source execution. Gettext
is fixed to identity and terminal width to 80; source argument logic is unchanged.
These original definitions, not a second hand-written parser, are the oracle.

Both actual offline native probes accept `parse_only=true` and report the full
public `parse_args` meaning: path tokens, absence/empty/content, alias, flags,
output and syntax failures. Only argv/id are sent, never expectations. There
are 60 source cases, not 120 independent cases because two probes observe them.
The original native parser disagreed in 20 cases before the bounded changes.

`consumers.json` contains **16 additional source-verified command-helper cases**:
four flat reads, five workspace/folder reads and seven GET reads. Seven GETs
run through both native probes, giving **23 native consumer observations**.
The folder probe uses the actual shared HTTP executor with a caller-injected
Azure HTTP client, credential and timer; the standard read probe retains its
existing SDK executor seam. Both run the actual native query and output.

Consumer source execution feeds the real registration's Namespace into the
unchanged source-executing read/folder references. GET's actual `process_nargs`
preprocessing runs before the metadata/query helper boundary. The old generic
GET reference intentionally disabled its language provider; this new oracle
injects actual development `jmespath==1.0.1` at that collaborator seam. Original
search, scalar conversion, command helpers, request handling, output/streams,
exit selection and error definitions remain intact. No expected-output
projection callback is used. This is bounded metadata-helper composition,
**not full GET command or executable production-host parity**.

Expectations were checked against source before native consumer comparison.
Initial unfrozen expectations omitted source scalar-to-list output wrapping;
those assumptions were corrected from actual Python evidence, not from Rust.
The GET source collaborator was enabled with the real JMESPath library rather
than treating its deliberately disabled old provider as product behavior.
Both new fixture files are now independently blob-frozen. Old fixtures,
source pins, query goldens and native boundary overlays are unchanged.

Guard tests reject source drift, changed expectation bytes/content, dropped or
reordered native inventories, query/path/flag/error/stream/request corruption,
expected replay, and attempted network/process/import/sleep/file side effects,
including attempts caught by source code.

## Run

Use the integration owner's shared-host gate when applicable. All Python packages
are development-only; no shipped Rust dependency, feature or lockfile changes.
From `rust`, with the existing pinned toolchain:

```powershell
cargo build --offline --locked -p fabric-cli --examples
python -B tests\compatibility\query_arguments\reference.py --rust-probe target\debug\examples\read_probe.exe --folder-probe target\debug\examples\folder_read_probe.exe
python -B -m unittest discover -s tests\compatibility\query_arguments -p test_reference.py -v
```

Use executable paths appropriate for the host OS. The parent owns wiring this
new independent reference into CI; the existing nine-stage CI runner is
unchanged. A source-only invocation omits both probe arguments.

## Boundaries

The supported lexical inventory covers source scalar/variadic separation,
attached versus separate values, option stops, last-occurrence precedence,
short clusters, unambiguous read-option abbreviations, ASCII negative
integers/decimals, `--`, path interaction and query absence/empty values.
This is not complete argparse parity. Unicode decimal-number token
classification and unported Python options are outside this measured inventory.
The pre-existing preview help/version and global `--output_format` behavior
remain distinct: Python's subparser default can erase a root-only override,
whereas the preview propagates its explicit global override.

Legacy folder fixtures still measure handler-list query presence, not the
source scalar parser. Raw consumers use the explicitly separate `raw_argv=true`
probe path with no caller-side joining. See the
[query argument compatibility matrix](../../../docs/query-argument-compatibility.md)
for the host presence boundary and final evidence.
