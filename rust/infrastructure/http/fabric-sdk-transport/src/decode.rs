// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_compression::tokio::bufread::{GzipDecoder, ZlibDecoder};
use azure_core::{
    error::ErrorKind,
    http::{headers::Headers, response::PinnedStream},
};
use futures_util::TryStreamExt;
use std::{error::Error, fmt, io, pin::Pin};
use tokio::io::AsyncRead;
use tokio_util::io::{ReaderStream, StreamReader};

pub(crate) const CHUNK_BYTES: usize = 8 * 1024;

pub(crate) fn decoded(
    stream: PinnedStream,
    headers: &Headers,
    has_body: bool,
) -> azure_core::Result<PinnedStream> {
    let source = StreamReader::new(
        stream.map_err(|error| io::Error::other(UpstreamKind(sanitized_kind(error.kind())))),
    );
    let encoding = headers
        .get_optional_str(&"content-encoding".into())
        .unwrap_or("identity");
    let reader: Pin<Box<dyn AsyncRead + Send>> = if !has_body {
        Box::pin(source)
    } else if encoding.eq_ignore_ascii_case("gzip") {
        let mut decoder = GzipDecoder::new(source);
        decoder.multiple_members(true);
        Box::pin(decoder)
    } else if encoding.eq_ignore_ascii_case("deflate") {
        let mut decoder = ZlibDecoder::new(source);
        decoder.multiple_members(true);
        Box::pin(decoder)
    } else if encoding.eq_ignore_ascii_case("identity") {
        Box::pin(source)
    } else {
        return Err(azure_core::Error::with_message(
            ErrorKind::DataConversion,
            "unsupported HTTP content encoding",
        ));
    };
    Ok(Box::pin(
        ReaderStream::with_capacity(reader, CHUNK_BYTES).map_err(|error| {
            let kind = error
                .get_ref()
                .and_then(|source| source.downcast_ref::<UpstreamKind>())
                .map_or(ErrorKind::DataConversion, |source| source.0.clone());
            azure_core::Error::with_message(
                kind,
                "native HTTP response decoding or streaming failed",
            )
        }),
    ))
}

pub(crate) fn sanitized_kind(kind: &ErrorKind) -> ErrorKind {
    match kind {
        ErrorKind::Io => ErrorKind::Io,
        ErrorKind::DataConversion => ErrorKind::DataConversion,
        ErrorKind::Connection => ErrorKind::Connection,
        ErrorKind::Credential => ErrorKind::Credential,
        _ => ErrorKind::Other,
    }
}

struct UpstreamKind(ErrorKind);

impl fmt::Debug for UpstreamKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UpstreamKind(<redacted>)")
    }
}

impl fmt::Display for UpstreamKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("native HTTP upstream stream failed")
    }
}

impl Error for UpstreamKind {}
