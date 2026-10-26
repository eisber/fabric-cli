<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Shared Fabric operation policy

This SDK-layer prerequisite extracts the existing Notebook request/202 polling/
result policy into one reusable implementation. It adds no transport, identity
backend, runtime, TLS, proxy or HTTP retry stack.

`OperationSession` owns finite request/response/poll-delay bounds, explicit Fabric
selection and token acquisition, cancellation, uncertain mutation evidence,
validated operation/result links and terminal-state reconciliation. Callers supply
an Azure `TokenCredential`, `CredentialSelection`, `OperationExecutor`, limits and
cancellation future. The request's token is never discovered or persisted.
`check_cancelled().await` checks that same cancellation signal between
caller-owned network/filesystem phases. A pending signal returns immediately;
ready cancellation stays latched without polling its completed future again.
It preserves operation ID, mutation uncertainty and request budgets, performs no
credential or executor work, and does not cancel or roll back a remote operation.
For SDKs that have already dispatched a request, a fresh session's
`reconcile_accepted(response, completion)` accepts that actual 202 and runs the
same bounded GET polling/result policy without issuing another mutation. The
external initial request counts against the request budget. Known operation ID
and possible-mutation evidence survive immediate cancellation; initial response
bytes are bounded, and a used session cannot silently reset its budget.
`OperationError` and the resource error adapter retain exact parsed failure data
in `raw_service_error`, in addition to typed service fields and reconciliation
evidence. Display remains sanitized; consumers explicitly choose whether to
render service text. Python-mode HTTP error-code precedence is separate from
the unchanged strict Notebook policy.
The borrowed `OperationError::resource_error()` projection exposes the same
existing resource mapping without consuming the original typed error; CLI layers
can explicitly render that view while retaining reconciliation evidence.

`FabricOperation` accepts only fixed public Fabric `/v1/` paths and finite methods.
Its additive `with_source_read_operation_ids()` builder is restricted to explicit
GET plus `PythonSynchronous`. It permits response operation IDs containing one
to 256 ASCII letters/digits/hyphens/underscores so source CLI reads can reconcile
opaque IDs without rewriting them. Empty operation IDs on advertised operations,
controls, non-ASCII, separators, query/fragment/percent/dot/user-information
syntax and oversized IDs fail before polling. Headerless 202 still remains
accepted. Existing constructors, Notebook/mutation consumers and
`reconcile_accepted` remain GUID-strict unless this GET-only builder is selected;
no origin or shared polling policy is relaxed globally.
`CompletionPolicy::ReturnAccepted` preserves 202; `WaitForResult` retrieves the
actual operation result; `WaitForCompletion` waits without fetching a resource.
`PythonSynchronous` implements the pinned Python client's default `wait=True`:
headerless 202 stays accepted, operation headers trigger polling, `Completed` and
`Succeeded` are terminal, other statuses keep polling within the caller's bound,
and an advertised result is fetched. Without a result Location, or on
`OperationHasNoResult`, the original response body is retained with status 200.
An initial Location without an operation-ID header preserves the source's empty
`operations/` identifier rather than silently extracting a different ID.
Default polling is ten seconds for Python mode, one second for the unchanged
Notebook policy. Invalid integer Retry-After values preserve Python's ten-second
fallback and are surfaced through `warnings()` for the caller's debug logger.
Acceptance never becomes completed work, and a dropped/cancelled/lost mutation
response never authorizes replay. POST, PATCH and DELETE uncertainty is recorded
before their request future can first be polled.

The SDK facade's existing Notebook types and signatures remain unchanged.
Its private adapter converts typed requests/results/errors, and its endpoint
validation, definition preparation, listing and final Notebook validation remain
in the facade. There is only one mutation/poll/result loop. Existing independent
Notebook and HTTP/deployment contracts remain unchanged.

The CLI mutation backend is a thin consumer over `HttpExecutor::execute_fabric`
and `wait_for`; shared HTTP still owns deadlines, GET-only retries and body
streaming limits. No extra method replay, ambient credential provider or alternate
origin is installed. OneLake/storage and ARM require separately reviewed typed
transport boundaries and are not enabled by this Fabric-only operation policy.

The source policy was extracted from the Rust baseline
`cc908ce08617f92601aa2dd68622797ccbcde9e7`, `fabric-sdk/src/notebooks.rs`.
Generic resource endpoint/prompt/output semantics remain pinned to Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.

The focused resource operation's explicit `with_continuation_token` is passed
through the existing `FabricOperation::from_resource` adapter unchanged; the
engine does not acquire or remember command-local read query state. Ordinary
operations remain stateless and polled operation/result URLs are not decorated.
`examples/continuation_query.rs` and `tests/continuation_reference.py` compare
actual domain PATCH/unassign POST and GET/DELETE URLs/body/status against the
original `do_request`, including opaque tokens and query overwrite/encoding.
External Rust consumers also verify cancellation, audience, response/request
bounds, headerless acceptance, no poll leakage and no fresh-command inheritance.
The production operation engine/ARM behavior is unchanged.

The development-only `python_operation` example compares actual original
`_handle_fab_long_running_op`, `_poll_operation`, `_fetch_operation_result` and
`get_polling_interval` bodies, using synthetic network/time only. It neither
rewrites the existing Notebook oracles nor requires Python in production.
