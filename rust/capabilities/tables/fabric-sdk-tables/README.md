<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Table SDK

`TableClient` provides Delta schema reads, Lakehouse table loading and table
maintenance composition. Supply a credential and explicit Fabric and OneLake
selections for the same account and tenant.

Storage reads use the storage audience; load and maintenance use Fabric.
Selection metadata does not verify token claims. The library creates no
runtime, HTTP client, configuration store or temporary files.

## Schema and loading

`TablePath` contains resolved workspace/item IDs and an item-relative path.
`schema` consumes a complete bounded Delta-log listing, visits JSON names in
reverse lexicographic order and returns the first `metaData.schemaString`
action. Malformed newer logs are not skipped; missing metadata is
`InvalidDeltaTable`, not an empty schema.

`TableWithOneLake` uses the generic OneLake executor for listing, reads and
properties. All pages count against the table request budget.

`source_kind` inspects source properties before format validation.
`load_with_kind` avoids repeating that inspection; `load` composes both.
Load targets `/v1/workspaces/{workspace}/lakehouses/{item}/tables/{name}/load`.
Folder input sets recursive loading. Payload paths do not imply cross-item
service authorization or load support.

`LoadOutcome` distinguishes confirmed completion, acceptance and silent
responses. Completion delegates to shared Fabric operation reconciliation
without another mutation or polling engine.

## Maintenance

`Maintenance::execution_data` creates vacuum/optimize job settings.
`JobMaintenance` delegates submission, status, waiting and cancellation to
`fabric-sdk-jobs`. `run_observed_for_job` accepts an explicitly selected job
type; the convenience APIs use table maintenance.

Wait options, timeout cancellation and a shared operation reconciler are
caller inputs. Local cancellation does not issue remote cancellation.
Accepted cancellation is not terminal completion.

Typed events retain run IDs and complete status rows. `TableError` keeps
original operation, job and storage failures with uncertain effects.
Observer failure stops local work without erasing evidence or replaying writes.
