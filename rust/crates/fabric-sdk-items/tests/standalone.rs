// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Downstream consumer with no facade, identity, deployment or CLI dependency.

use fabric_sdk_items::{
    CredentialSelection, ItemClient as ConsumerClient, ItemExecutionError, ItemExecutor,
    ItemGetOptions, ItemListOptions, ItemRequest, ItemResponse, TokenCredential,
};

include!("../../../tests/compatibility/items/consumer.rs");
