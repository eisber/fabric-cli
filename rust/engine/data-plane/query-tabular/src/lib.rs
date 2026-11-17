// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Schema-driven conversion of completed native query results into shared Arrow tables.
//!
//! No dispatch, credentials, files, serialization implementation, or dataset
//! selection is hidden here. Use [`fabric_sdk_tabular::encode`] on an explicitly
//! selected dataset. Conversion never returns a clipped or partially converted result.
//! Enable `aihub` to convert validated native DataAgent results through the same codecs.

#[cfg(feature = "aihub")]
mod aihub;
mod bounds;
mod error;
mod kql;
mod scalar;
mod table;
#[cfg(feature = "aihub")]
mod typed_scalar;

#[cfg(feature = "aihub")]
pub use aihub::{AihubDataset, AihubTabularResult, convert_aihub};
pub use bounds::{ConversionControl, ConversionLimits};
pub use error::{ConversionError, ConversionLimit};
pub use kql::{KqlDataset, KqlTabularResult, convert_kql};
