// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Engine-neutral, bounded Arrow results and exact CSV, JSON, and Parquet output.
//!
//! A result contains exactly one ordered dataset. IPC decoding requires an explicit
//! end marker and rejects trailing datasets, diagnostic metadata, compression, and
//! unsupported types before materializing record batches. See the crate README
//! for the supported type matrix, metadata policy, and textual encodings.
//!
//! This crate performs no file, network, identity, runtime, or terminal operations.

#![forbid(unsafe_code)]

mod encode;
mod error;
mod ipc;
mod result;

pub use encode::{ResultFormat, encode, encode_to_writer};
pub use error::{
    LimitKind, MetadataClassification, MetadataEvidence, MetadataLocation, TabularError,
};
pub use ipc::decode_arrow_stream;
pub use result::{OutputLimits, TabularResult};
