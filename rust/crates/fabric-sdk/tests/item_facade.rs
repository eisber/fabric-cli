// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The same consumer contract through stable facade imports and delegation.

use fabric_sdk::{
    Client as ConsumerClient, CredentialSelection, TokenCredential,
    items::{
        Item, ItemExecutionError, ItemExecutor, ItemGetOptions, ItemListOptions, ItemMetadata,
        ItemRequest, ItemResponse,
    },
};

include!("../../../tests/compatibility/items/consumer.rs");
include!("../../../tests/compatibility/items/metadata_consumer.rs");
