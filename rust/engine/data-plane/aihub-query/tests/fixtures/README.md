<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Inert native workbook fixture

`inert-native-workbook.xlsx` is an independently hand-authored, deterministic
7,524-byte structural test input, not a workload download or product-generated
expected result. SHA-256:
`1896b616bf57300a37bd7918ab419e31a01e01a97d440c4cdd20ef601ec22f6c`.

The 12 stored ZIP parts contain a workbook-local
`Microsoft.Mashup.OleDb.1` connection with ID `1`, a query table linked to that
connection, a cached synthetic row, and a DataMashup section containing inert
local `#table` M. The synthetic fields are `SyntheticId` (Int64) and `Product`
(text), with the row `1`, `Orion sample`. An opaque
`customXml/passthrough-sentinel.xml` part detects unintended repackaging.
There are no remote connectors, service endpoints, tenants, accounts,
credentials or MIP labels.

Independent fixture preparation checked CRCs, XML, DataMashup base64/inner
package, connection/query-table IDs and cached values. Empty auxiliary Mashup
sections are intentional. This fixture establishes byte-preserving handling of
a query-carrying package, **not** Excel desktop refresh or complete Excel
interoperability. Real workload package qualification is separate private
evidence; no live service workbook is committed.

SDK tests pass these immutable bytes through a synthetic MCP response. Root
tests additionally compare actual standard `FileSink` output with the original
fixture. Neither test generates its expected workbook using product code or
evaluates the embedded M. Opaque non-ZIP byte tests separately ensure that
future protected workbook contents are not rejected by file-magic guessing.
