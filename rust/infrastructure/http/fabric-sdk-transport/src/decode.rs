// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_compression::tokio::bufread::{GzipDecoder, ZlibDecoder};
use azure_core::{
    error::ErrorKind,
    http::{headers::Headers, response::PinnedStream},
};
use futures_util::TryStreamExt;
use std::{io, pin::Pin};
use tokio::io::AsyncRead;
use tokio_util::io::{ReaderStream, StreamReader};

pub(crate) const CHUNK_BYTES: usize = 8 * 1024;

pub(crate) fn decoded(
    stream: PinnedStream,
    headers: &Headers,
    has_body: bool,
) -> azure_core::Result<PinnedStream> {
    let source = StreamReader::new(
        stream.map_err(|_| io::Error::other("native HTTP response stream failed")),
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
        ReaderStream::with_capacity(reader, CHUNK_BYTES).map_err(|_| {
            azure_core::Error::with_message(
                ErrorKind::Io,
                "native HTTP response decoding or streaming failed",
            )
        }),
    ))
}
