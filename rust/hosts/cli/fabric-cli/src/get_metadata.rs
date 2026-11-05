// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in metadata GET composition, separate from the Python-compatible handler.
//!
//! These are the SDK's shared operation types, not duplicate CLI target or result
//! contracts. Hosts resolve targets and select envelopes, streams and local effects.
//! The existing executable's GET grammar and behavior are unchanged.

pub use fabric_sdk::metadata::{
    MetadataError, MetadataHost, MetadataLimits, MetadataRead, MetadataTarget,
};

use fabric_cli_storage_files::{ExactOutputError, ExactOutputFile, ExactOutputReceipt};
use std::{future::Future, path::Path};

/// A preflighted exact-file metadata request.
///
/// Construct in the caller's blocking context before acquiring remote content.
/// Acquisition is async; the final file commit is a separate blocking boundary.
/// No runtime or blocking worker is created by this library.
#[derive(Debug)]
pub struct MetadataFileRequest {
    output: ExactOutputFile,
    limits: MetadataLimits,
}

impl MetadataFileRequest {
    /// Validate one resolved absolute output filename without creating it.
    ///
    /// The output cap is the same unprojected serialized-data bound as the SDK
    /// read. No destination existence failure can reach remote content reads.
    ///
    /// # Errors
    /// Preserves the exact-output adapter's typed preflight failures.
    pub fn preflight(path: &Path, limits: MetadataLimits) -> Result<Self, ExactOutputError> {
        Ok(Self {
            output: ExactOutputFile::preflight(path, limits.max_result_bytes.get())?,
            limits,
        })
    }

    /// Acquire complete bounded metadata without creating the output file.
    ///
    /// # Errors
    /// Preserves SDK read, cancellation and budget failures. A failed acquisition
    /// cannot produce a file or a successful-looking prefix.
    pub async fn acquire<'target>(
        self,
        host: &MetadataHost<'_>,
        target: MetadataTarget<'target>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<MetadataFileOutput<'target>, MetadataError> {
        let read = host.read(target, self.limits, cancelled).await?;
        Ok(MetadataFileOutput {
            output: self.output,
            read,
        })
    }
}

/// Complete acquired metadata, awaiting explicit local delivery.
///
/// The caller may inspect data or abandon delivery without creating a file.
/// If the caller is cancelled after acquisition, it must not start commit.
#[derive(Debug)]
pub struct MetadataFileOutput<'a> {
    output: ExactOutputFile,
    read: MetadataRead<'a>,
}

impl<'a> MetadataFileOutput<'a> {
    /// Inspect the same SDK result exposed to non-CLI consumers.
    pub fn read(&self) -> &MetadataRead<'a> {
        &self.read
    }

    /// Write the exact bounded JSON data bytes in the caller's blocking context.
    ///
    /// No remote call, envelope, newline, directory or suffix is added. A racing
    /// destination is never overwritten. Once the final file is created, a local
    /// write failure can leave partial effects; cancellation is not rollback.
    ///
    /// # Errors
    /// Preserves creation status, bytes written, stage and native I/O evidence.
    pub fn commit(self) -> Result<ExactOutputReceipt, ExactOutputError> {
        self.output.commit(self.read.json_bytes())
    }
}
