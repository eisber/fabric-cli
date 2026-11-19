// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{PreparationError, ResolvedRoute};
use std::{fmt, io};

/// Existing root query statement sources, without a new parser or grammar.
#[derive(Clone)]
pub enum TextSource {
    /// Exact inline text, with no expansion or unescaping.
    Inline(String),
    /// Exact filename spelling; `-` denotes the one stdin reader.
    File(String),
}

impl fmt::Debug for TextSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TextSource(<redacted>)")
    }
}

/// Frozen query argument seam, independent of the app/parser dependency.
pub struct QueryInput {
    /// Exactly one statement source.
    pub text: TextSource,
    /// Existing optional native parameters filename.
    pub parameters: Option<String>,
}

/// Host-owned input selected without opening any ambient path in this package.
pub enum InputSource<'a> {
    /// Exact caller-supplied filename.
    File(&'a str),
    /// The one command-local standard-input reader.
    Stdin,
}

/// Injected, bounded reader; implementations must stop after `max_bytes + 1`.
pub trait InputReader {
    /// Read at most one byte beyond the limit so excess input is detectable.
    fn read(&mut self, source: InputSource<'_>, max_bytes: usize) -> io::Result<Vec<u8>>;
}

/// Exact validated statement, without provider, credential, or output discovery.
pub struct PreparedInput {
    text: String,
}

impl PreparedInput {
    /// Original query bytes, never rewritten or included in diagnostics.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Debug for PreparedInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedInput(<redacted>)")
    }
}

/// Resolve local input once, after selecting one provider and before credentials.
///
/// Unsupported native parameters are rejected before opening any reader. This
/// package does not treat AIHub's curated-template `language` as a query engine
/// selector or interpret parameter JSON as an output projection.
pub fn prepare_input(
    input: QueryInput,
    route: &ResolvedRoute,
    reader: &mut dyn InputReader,
) -> Result<PreparedInput, PreparationError> {
    let value = match &input.text {
        TextSource::Inline(value) | TextSource::File(value) => value,
    };
    if value.trim().is_empty()
        || input
            .parameters
            .as_ref()
            .is_some_and(|path| path.trim().is_empty())
        || matches!(&input.text, TextSource::File(path) if path == "-")
            && input.parameters.as_deref() == Some("-")
    {
        return Err(PreparationError::InvalidInput);
    }
    if input.parameters.is_some() {
        return Err(PreparationError::UnsupportedParameters);
    }
    let max_bytes = route.max_statement_bytes();
    let text = match input.text {
        TextSource::Inline(text) => text,
        TextSource::File(path) => {
            let source = if path == "-" {
                InputSource::Stdin
            } else {
                InputSource::File(&path)
            };
            let bytes = reader
                .read(source, max_bytes)
                .map_err(|_| PreparationError::InputRead)?;
            if bytes.len() > max_bytes {
                return Err(PreparationError::InvalidInput);
            }
            String::from_utf8(bytes).map_err(|_| PreparationError::InvalidInput)?
        }
    };
    if text.trim().is_empty() || text.contains('\0') || text.len() > max_bytes {
        return Err(PreparationError::InvalidInput);
    }
    Ok(PreparedInput { text })
}
