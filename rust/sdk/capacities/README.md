<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric capacity reads

`fabric-sdk-capacities` provides bounded `GET /v1/capacities` listing through
`CapacityClient`. The SDK facade exposes the same operation as
`Client::list_capacities`.

## Caller contract

Supply a credential bound to the explicit account/tenant selection, a
`CapacityExecutor`, limits and cancellation. Construction acquires no token.
Requests use the Fabric scope `https://api.fabric.microsoft.com/.default`.

`CapacityClient::new` selects public Fabric. `with_context(RequestContext)`
accepts an explicit endpoint; initial and continuation requests retain that
binding. Configure the HTTP executor with the same endpoint.

```rust,no_run
use fabric_sdk_capacities::{
    CapacityClient, CapacityExecutor, CapacityListError, CapacityListLimits,
};

async fn list(
    client: &CapacityClient,
    executor: &dyn CapacityExecutor,
) -> Result<usize, CapacityListError> {
    let result = client.list_capacities(
        executor,
        CapacityListLimits::default(),
        std::future::pending(),
    ).await?;
    Ok(result.value.len())
}
```

## Execution and limits

Paging preserves row order and duplicates. Continuations, response size,
request budgets and cancellation are validated explicitly; errors retain
partial progress instead of returning a complete-looking list.

`fabric-sdk-http::HttpExecutor` implements `CapacityExecutor` with shared
streaming bounds, deadlines and GET retry policy. This crate installs no
transport, runtime or identity backend.

ARM subscription discovery and capacity creation, update, deletion, resume or
suspend are separate APIs and are not provided here.
