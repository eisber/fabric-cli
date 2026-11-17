// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One explicit bounded binary file/stdin reader, reused by both query entrypoints.

use fabric_cli::{
    native_tabular::{NativeCommand, Operation, TextSource},
    query_host::QueryHostError,
};
use fabric_cli_query_commands::{InputReader, InputSource, PreparationError};
use std::io::{self, Read};

const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct CachedInput {
    source: Option<String>,
    bytes: Option<Vec<u8>>,
}

pub(crate) fn read(command: &NativeCommand) -> Result<CachedInput, QueryHostError> {
    let Operation::Query(options) = &command.operation else {
        return Err(QueryHostError::WrongInvocation);
    };
    if options.parameters.is_some() {
        return Err(PreparationError::UnsupportedParameters.into());
    }
    let TextSource::File(path) = &options.text else {
        let TextSource::Inline(text) = &options.text else {
            unreachable!()
        };
        if text.trim().is_empty() || text.contains('\0') || text.len() > MAX_INPUT_BYTES {
            return Err(PreparationError::InvalidInput.into());
        }
        return Ok(CachedInput {
            source: None,
            bytes: None,
        });
    };
    if path.trim().is_empty() {
        return Err(PreparationError::InvalidInput.into());
    }
    let mut reader: Box<dyn Read> = if path == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(std::fs::File::open(path).map_err(|_| PreparationError::InputRead)?)
    };
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| PreparationError::InputRead)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(PreparationError::InvalidInput.into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| PreparationError::InvalidInput)?;
    if text.trim().is_empty() || text.contains('\0') {
        return Err(PreparationError::InvalidInput.into());
    }
    Ok(CachedInput {
        source: Some(path.clone()),
        bytes: Some(bytes),
    })
}

impl InputReader for CachedInput {
    fn read(&mut self, source: InputSource<'_>, max_bytes: usize) -> io::Result<Vec<u8>> {
        let matches = match source {
            InputSource::Stdin => self.source.as_deref() == Some("-"),
            InputSource::File(path) => self.source.as_deref() == Some(path),
        };
        if !matches {
            return Err(io::Error::other("Query input source changed"));
        }
        let mut bytes = self
            .bytes
            .take()
            .ok_or_else(|| io::Error::other("Query input was already consumed"))?;
        let probe = max_bytes
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Invalid finite query input bound"))?;
        // A selected provider with a smaller bound receives only the standard
        // overflow probe; prepare_input rejects it before any query dispatch.
        bytes.truncate(probe);
        Ok(bytes)
    }
}
