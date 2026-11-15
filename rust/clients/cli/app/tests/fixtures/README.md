<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent Python oracle

The envelope fixtures were captured by executing `OutputStatus`, `OutputResult`,
and `FabricCLIOutput` from pinned upstream commit
`5b504195c63ef54c7382ca8e8a9d9759717f0ec5`, before comparing Rust output.
The source classes were loaded with AST extraction and the timestamp fixed to
`2000-01-01T00:00:00Z`; Python `json.dumps(..., indent=4)` supplied the bytes.
They were not generated from Rust. Both files include the print newline.

The host tests additionally assert independent cases from pinned
`commands/api/fab_api_request.py` and `utils/fab_jmespath.py`: the query sees the
full status/headers/text object; scalar false, null, and zero become Python
strings; projected lists are not wrapped again by `OutputResult`. Unlike Python,
Rust preserves empty projected collections/strings instead of returning the
full response. Whitespace bodies become
`"(Empty)"`; invalid nonempty JSON is an error.

Additional integer expectations in the host/shared-JSON tests come from Python
`json.loads`/`json.dumps`, not from Rust serialization. They cover `u64`/`i64`
boundaries, both signs outside those ranges, larger-than-`i128` values, scalar and
container projection, and finite-float normalization. The CLI's empty-query
bypass follows `if query` in pinned `fab_api_request.py`; empty-query inventory
remains a separate query-library behavior. Pinned `fab_api.py` maps JSON-file
`null` to absent input, while `fab_api_client.py` supplies JSON Content-Type even
without a body. Exact prepared-request tests cover these cases and overrides.

Reserved-key fixtures also use independent Python JSON expectations. They retain
root, nested, and escaped `$serde_json::private::Number` keys with numeric and
nonnumeric strings, alongside exact wide integers. API output assertions use
typed raw JSON maps rather than deserializing `Value` or reusing the decoder
under test, so serde's marker interpretation cannot hide a regression.

Coverage limit: these byte fixtures cover the envelope and ordered JSON body,
not HTTP wire-header ordering. The agreed shared `ApiResponse` uses a
`BTreeMap`, so displayed header keys are sorted rather than retaining wire
insertion order. Header values and visibility are tested separately.

Approved interface differences: long-only API switches, `--show-headers` rather
than `--show_headers`, no `--params` (query text stays in the endpoint), required
Rust binding, missing input files fail explicitly, default recognized-LRO
polling, `--async` returning the actual initial response once, and preserving
empty query results. These are not claims of Python flag or default-polling
parity.
