// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Downstream consumer with no facade, identity, deployment or CLI dependency.

use fabric_sdk_items::{
    CredentialSelection, Item, ItemClient as ConsumerClient, ItemCollection, ItemErrorKind,
    ItemExecutionError, ItemExecutor, ItemGetOptions, ItemListOptions, ItemMetadata, ItemRequest,
    ItemResponse, TokenCredential,
};

include!("../../../tests/compatibility/items/consumer.rs");
include!("../../../tests/compatibility/items/metadata_consumer.rs");
include!("../../../tests/compatibility/items/specialized_consumer.rs");
