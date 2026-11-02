<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# REST client implementation decision

**Status: accepted by the owner on October 24, 2026.** Implement typed REST
operations by hand over shared HTTP infrastructure and behavioral policies.
Do not adopt OpenAPI Generator, Progenitor, or Azure TypeSpec Rust generation
for the production SDK. Retain the isolated experiments as evidence, not as
production dependencies.

## Rationale and evidence

The owner reviewed the implementation and selected handwritten operations after
the four-operation comparison. The experiment used public Rust checkpoint
`e2680777073d529b6ad10e47973ff601f2bd2c89` and public Fabric REST specifications
`a96c37a56b098e55be3ed09e07eccdc5f39adb7a`.

The handwritten implementation and two generated-wire hybrids each passed the
same 35 offline cases. Both hybrids retained the existing handwritten response
models and behavioral policies. Generation therefore did not eliminate that
maintenance. Stock clients needed adaptations, and the TypeSpec experiment
established library compilation, not equivalent runtime behavior.

This is a scope and maintenance decision, not a claim that handwritten code is
universally faster, smaller, or less error-prone. The four-operation experiment
did not establish a reliable runtime winner or long-term savings across the
whole API surface. The internal evidence record is `tk:wiki/12/905`, with the
measured summary at `tk:post/12/4/47`.

## Implementation boundary

- Keep endpoint-specific code focused on method, route, query, request/response
  models, success statuses, and explicitly documented service exceptions.
- Reuse shared credential injection, transport, bounded response reading,
  deadlines, cancellation, sanitized errors, pagination, and long-running
  operation machinery. Do not duplicate these policies per endpoint.
- Use established HTTP libraries rather than implementing an HTTP/TLS stack.
  This decision does not select a production transport, TLS configuration,
  authentication backend, retry policy, or new dependency.
- Preserve operation-specific retry and reconciliation rules. In particular,
  losing a mutation response or cancelling a request is not permission to
  replay the mutation or report success.
- Keep service behavior in SDK-layer crates, deployment orchestration in the
  deployment layer, and argument/output handling in the CLI. Preserve existing
  public facade imports and caller-injected credentials.
- Translate the approved source behavior first and retain the independent
  compatibility oracle, explicit exceptions, and offline boundary tests.

## Specifications and future changes

Use pinned public Swagger/OpenAPI specifications as contract and review inputs.
Record differences between the schema, reference implementation, and approved
Rust behavior rather than silently tightening models or changing expectations.
Specification-driven validation can be proposed independently; no new generator
or drift-check pipeline is required by this decision.

Do not introduce speculative wrappers for every published API. Implement the
operations required by approved SDK/CLI/deployment slices, with their consumers
and failure paths.

Reconsider generation only through a separate owner decision supported by
measured recurring maintenance costs or a materially changed upstream toolchain.
An authoritative upstream TypeSpec source could change those economics, but
acceptance of TypeSpec upstream would not automatically reverse this decision.
