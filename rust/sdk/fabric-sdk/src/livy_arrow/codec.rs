// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::ArrowError;
use std::collections::HashSet;
use std::io::Write;

/// Encoded-stream bounds, checked before reserving ID or payload storage.
///
/// Payload totals exclude framing; wire totals additionally bound framing.
/// Bookkeeping retains IDs/positions, not old payloads. Allocator overhead and
/// application-retained chunks are outside these bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLimits {
    /// Maximum encoded payload bytes in one frame.
    pub max_frame_bytes: usize,
    /// Maximum cumulative encoded payload bytes per response/logical delivery.
    pub max_total_bytes: u64,
    /// Maximum frames per response/unique delivered chunks per logical delivery.
    pub max_chunks: usize,
    /// Maximum UTF-8 bytes in one ID; never greater than protocol maximum 65536.
    pub max_id_bytes: usize,
    /// Maximum sum of retained ID bytes.
    pub max_identity_bytes: usize,
    /// Maximum HTTP body bytes, including headers, IDs, replay and terminal.
    pub max_wire_bytes: u64,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 256 * 1024 * 1024,
            max_total_bytes: 1 << 40,
            max_chunks: 100_000,
            max_id_bytes: 65_536,
            max_identity_bytes: 16 * 1024 * 1024,
            max_wire_bytes: (1 << 40) + 32 * 1024 * 1024,
        }
    }
}

impl StreamLimits {
    /// Validate strictly positive finite bounds without allocating.
    pub fn validate(self) -> Result<Self, ArrowError> {
        if self.max_frame_bytes == 0
            || self.max_frame_bytes > i64::MAX as usize
            || self.max_total_bytes == 0
            || self.max_chunks == 0
            || self.max_id_bytes == 0
            || self.max_id_bytes > 65_536
            || self.max_identity_bytes == 0
            || self.max_wire_bytes < 8
        {
            return Err(ArrowError::InvalidInput);
        }
        Ok(self)
    }
}

/// One complete encoded IPC payload; raw APIs do not interpret IPC or schema.
#[derive(Clone, Eq, PartialEq)]
pub struct RawChunk {
    partition: i32,
    ordinal: i32,
    id: String,
    payload: Vec<u8>,
}

impl std::fmt::Debug for RawChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawChunk")
            .field("partition", &self.partition)
            .field("ordinal", &self.ordinal)
            .field("id", &"<redacted>")
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

impl RawChunk {
    /// Validate an already-owned chunk against finite per-chunk bounds.
    ///
    /// The caller allocated these arguments; streaming consumers should use
    /// [`FramedDecoder`] to enforce bounds before allocation.
    pub fn new(
        partition: i32,
        ordinal: i32,
        id: String,
        payload: Vec<u8>,
        limits: StreamLimits,
    ) -> Result<Self, ArrowError> {
        limits.validate()?;
        if payload.is_empty() || partition < 0 || ordinal < 0 {
            return Err(ArrowError::Protocol);
        }
        if payload.len() > limits.max_frame_bytes
            || payload.len() as u64 > limits.max_total_bytes
            || id.len() > limits.max_id_bytes
            || id.len() > limits.max_identity_bytes
        {
            return Err(ArrowError::Limit);
        }
        validate_id(id.as_bytes(), partition)?;
        Ok(Self {
            partition,
            ordinal,
            id,
            payload,
        })
    }

    /// Owning nonnegative partition index.
    pub const fn partition(&self) -> i32 {
        self.partition
    }
    /// Nonnegative ordinal; not necessarily contiguous, sorted or equal to ID sequence.
    pub const fn ordinal(&self) -> i32 {
        self.ordinal
    }
    /// Validated UTF-8 identity.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Encoded payload, not a decoded batch or HTTP fragment.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

pub(super) fn validate_id(id: &[u8], partition: i32) -> Result<(), ArrowError> {
    let text = std::str::from_utf8(id).map_err(|_| ArrowError::Protocol)?;
    let remainder = text.strip_prefix("part-").ok_or(ArrowError::Protocol)?;
    let (part, sequence) = remainder.split_once('-').ok_or(ArrowError::Protocol)?;
    if part.is_empty() || sequence.is_empty() {
        return Err(ArrowError::Protocol);
    }
    // The source regex accepts Unicode decimal digits as well as ASCII.
    // Python's \d and int() use Unicode Nd, not all numeric characters.
    let mut value = 0_i32;
    for digit in part.chars() {
        let digit = decimal_digit(digit).ok_or(ArrowError::Protocol)?;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
            .ok_or(ArrowError::Protocol)?;
    }
    if value != partition || sequence.chars().any(|digit| decimal_digit(digit).is_none()) {
        return Err(ArrowError::Protocol);
    }
    Ok(())
}

fn decimal_digit(character: char) -> Option<i32> {
    // Unicode 16.0 decimal blocks (Rust char::to_digit only recognizes ASCII).
    // Consecutive mathematical digits cover five separate ten-digit sets.
    const ZEROES: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66,
        0xce6, 0xd66, 0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810,
        0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620,
        0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30,
        0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
        0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0,
        0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50,
        0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0,
        0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    let code = u32::from(character);
    ZEROES.iter().find_map(|zero| {
        code.checked_sub(*zero)
            .filter(|digit| *digit < 10)
            .map(|digit| digit as i32)
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Length,
    Metadata,
    Id,
    Payload,
    Terminal,
    Finished,
}

/// Incremental one-slot framed-v2 parser with explicit backpressure.
///
/// Call `push` with arbitrary fragments; retain its unconsumed suffix. When
/// `drain` yields a chunk, deliver it before reading more HTTP bytes. `push`
/// consumes zero while that slot is occupied. `finish` must be called at HTTP
/// EOF: observing a zero marker alone is not successful completion.
pub struct FramedDecoder {
    limits: StreamLimits,
    phase: Phase,
    header: [u8; 20],
    filled: usize,
    payload_length: usize,
    partition: i32,
    ordinal: i32,
    id_length: usize,
    id: Vec<u8>,
    payload: Vec<u8>,
    pending: Option<RawChunk>,
    seen: HashSet<String>,
    orders: HashSet<(i32, i32)>,
    payload_total: u64,
    identity_total: usize,
    wire_total: u64,
    error: Option<ArrowError>,
}

impl FramedDecoder {
    /// Construct a fresh parser for one HTTP response after header validation.
    pub fn new(limits: StreamLimits) -> Result<Self, ArrowError> {
        Ok(Self {
            limits: limits.validate()?,
            phase: Phase::Length,
            header: [0; 20],
            filled: 0,
            payload_length: 0,
            partition: 0,
            ordinal: 0,
            id_length: 0,
            id: Vec::new(),
            payload: Vec::new(),
            pending: None,
            seen: HashSet::new(),
            orders: HashSet::new(),
            payload_total: 0,
            identity_total: 0,
            wire_total: 0,
            error: None,
        })
    }

    /// Consume at most one complete frame, returning the exact consumed prefix.
    ///
    /// All errors poison this response parser; discard it before reconnecting.
    pub fn push(&mut self, input: &[u8]) -> Result<usize, ArrowError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = self.push_inner(input);
        if let Err(error) = result {
            self.error = Some(error);
        }
        result
    }

    fn push_inner(&mut self, input: &[u8]) -> Result<usize, ArrowError> {
        if self.phase == Phase::Finished {
            return Err(ArrowError::InvalidState);
        }
        let mut consumed = 0;
        while consumed < input.len() && self.pending.is_none() {
            let remaining = &input[consumed..];
            let count = match self.phase {
                Phase::Length | Phase::Metadata => {
                    let end = if self.phase == Phase::Length { 8 } else { 20 };
                    let count = remaining.len().min(end - self.filled);
                    self.charge_wire(count)?;
                    self.header[self.filled..self.filled + count]
                        .copy_from_slice(&remaining[..count]);
                    self.filled += count;
                    if self.filled == end {
                        if self.phase == Phase::Length {
                            self.read_length()?;
                        } else {
                            self.read_metadata()?;
                        }
                    }
                    count
                }
                Phase::Id => {
                    let count = remaining.len().min(self.id_length - self.id.len());
                    self.charge_wire(count)?;
                    self.id.extend_from_slice(&remaining[..count]);
                    if self.id.len() == self.id_length {
                        self.read_id()?;
                    }
                    count
                }
                Phase::Payload => {
                    let count = remaining
                        .len()
                        .min(self.payload_length - self.payload.len());
                    self.charge_wire(count)?;
                    self.payload.extend_from_slice(&remaining[..count]);
                    if self.payload.len() == self.payload_length {
                        let id = String::from_utf8(std::mem::take(&mut self.id))
                            .map_err(|_| ArrowError::Protocol)?;
                        self.pending = Some(RawChunk {
                            partition: self.partition,
                            ordinal: self.ordinal,
                            id,
                            payload: std::mem::take(&mut self.payload),
                        });
                        self.phase = Phase::Length;
                        self.filled = 0;
                    }
                    count
                }
                Phase::Terminal => return Err(ArrowError::TrailingBytes),
                Phase::Finished => return Err(ArrowError::InvalidState),
            };
            consumed += count;
        }
        Ok(consumed)
    }

    fn charge_wire(&mut self, bytes: usize) -> Result<(), ArrowError> {
        self.wire_total = self
            .wire_total
            .checked_add(bytes as u64)
            .filter(|total| *total <= self.limits.max_wire_bytes)
            .ok_or(ArrowError::Limit)?;
        Ok(())
    }

    fn read_length(&mut self) -> Result<(), ArrowError> {
        let length = i64::from_be_bytes(self.header[..8].try_into().map_err(|_| ArrowError::Protocol)?);
        match length {
            0 => self.phase = Phase::Terminal,
            -1 => return Err(ArrowError::ServiceFailure),
            length if length < 0 => return Err(ArrowError::Protocol),
            length => {
                self.payload_length = usize::try_from(length).map_err(|_| ArrowError::Limit)?;
                if self.payload_length > self.limits.max_frame_bytes
                    || self.seen.len() >= self.limits.max_chunks
                {
                    return Err(ArrowError::Limit);
                }
                self.payload_total = self
                    .payload_total
                    .checked_add(length as u64)
                    .filter(|total| *total <= self.limits.max_total_bytes)
                    .ok_or(ArrowError::Limit)?;
                self.phase = Phase::Metadata;
            }
        }
        Ok(())
    }

    fn read_metadata(&mut self) -> Result<(), ArrowError> {
        self.partition = i32::from_be_bytes(self.header[8..12].try_into().map_err(|_| ArrowError::Protocol)?);
        self.ordinal = i32::from_be_bytes(self.header[12..16].try_into().map_err(|_| ArrowError::Protocol)?);
        let length = i32::from_be_bytes(self.header[16..20].try_into().map_err(|_| ArrowError::Protocol)?);
        if self.partition < 0 || self.ordinal < 0 || length <= 0 {
            return Err(ArrowError::Protocol);
        }
        self.id_length = length as usize;
        if self.id_length > 65_536 {
            return Err(ArrowError::Protocol);
        }
        if self.id_length > self.limits.max_id_bytes {
            return Err(ArrowError::Limit);
        }
        self.identity_total = self
            .identity_total
            .checked_add(self.id_length)
            .filter(|total| *total <= self.limits.max_identity_bytes)
            .ok_or(ArrowError::Limit)?;
        // Even a declared complete frame must fit the remaining wire budget.
        let needed = (self.id_length as u64)
            .checked_add(self.payload_length as u64)
            .ok_or(ArrowError::Limit)?;
        if needed > self.limits.max_wire_bytes - self.wire_total {
            return Err(ArrowError::Limit);
        }
        self.id.try_reserve_exact(self.id_length).map_err(|_| ArrowError::Allocation)?;
        self.phase = Phase::Id;
        Ok(())
    }

    fn read_id(&mut self) -> Result<(), ArrowError> {
        validate_id(&self.id, self.partition)?;
        let text = std::str::from_utf8(&self.id).map_err(|_| ArrowError::Protocol)?;
        if self.seen.contains(text) {
            return Err(ArrowError::DuplicateId);
        }
        if self.orders.contains(&(self.partition, self.ordinal)) {
            return Err(ArrowError::OrdinalConflict);
        }
        self.seen.try_reserve(1).map_err(|_| ArrowError::Allocation)?;
        self.orders.try_reserve(1).map_err(|_| ArrowError::Allocation)?;
        let mut stored = String::new();
        stored.try_reserve_exact(text.len()).map_err(|_| ArrowError::Allocation)?;
        stored.push_str(text);
        self.seen.insert(stored);
        self.orders.insert((self.partition, self.ordinal));
        self.payload.try_reserve_exact(self.payload_length).map_err(|_| ArrowError::Allocation)?;
        self.phase = Phase::Payload;
        Ok(())
    }

    /// Take the single fully assembled chunk, releasing parser backpressure.
    pub fn drain(&mut self) -> Option<RawChunk> {
        self.pending.take()
    }

    /// Validate HTTP EOF after draining every chunk.
    ///
    /// No terminal means truncation, even with no partial header remaining.
    pub fn finish(&mut self) -> Result<(), ArrowError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.pending.is_some() {
            return Err(ArrowError::InvalidState);
        }
        if self.phase == Phase::Finished {
            return Ok(());
        }
        if self.phase != Phase::Terminal {
            self.error = Some(ArrowError::Truncated);
            return Err(ArrowError::Truncated);
        }
        self.phase = Phase::Finished;
        Ok(())
    }

    /// Cumulative consumed wire bytes, useful for host accounting.
    pub const fn wire_bytes(&self) -> u64 {
        self.wire_total
    }
}

/// Bounded borrowed-byte pass-through for bare inline IPC, not framed-v2.
///
/// EOF is the HTTP body boundary. This does not inspect IPC terminal messages,
/// validate IPC, decompress, or imply that an empty body is a valid IPC stream.
pub struct RawInline {
    max_bytes: u64,
    total: u64,
    finished: bool,
    failed: bool,
}

impl RawInline {
    /// Start a raw inline body with a positive encoded-byte limit.
    pub fn new(max_bytes: u64) -> Result<Self, ArrowError> {
        if max_bytes == 0 {
            return Err(ArrowError::InvalidInput);
        }
        Ok(Self { max_bytes, total: 0, finished: false, failed: false })
    }

    /// Check a borrowed fragment before handing it to the caller's sink.
    pub fn consume<'a>(&mut self, bytes: &'a [u8]) -> Result<&'a [u8], ArrowError> {
        if self.finished || self.failed {
            return Err(ArrowError::InvalidState);
        }
        let Some(total) = self.total.checked_add(bytes.len() as u64).filter(|total| *total <= self.max_bytes) else {
            self.failed = true;
            return Err(ArrowError::Limit);
        };
        self.total = total;
        Ok(bytes)
    }

    /// Record HTTP EOF; raw mode deliberately makes no IPC-validity claim.
    pub fn finish(&mut self) -> Result<(), ArrowError> {
        if self.failed {
            return Err(ArrowError::InvalidState);
        }
        self.finished = true;
        Ok(())
    }
}

/// Write one raw framed-v2 frame, not one standard Arrow IPC file.
///
/// A failed write leaves a partial artifact. Only call [`write_terminal`] after
/// the source decoder has validated terminal plus EOF and all deliveries passed.
pub fn write_frame(writer: &mut impl Write, chunk: &RawChunk) -> Result<(), ArrowError> {
    let length = i64::try_from(chunk.payload.len()).map_err(|_| ArrowError::Limit)?;
    let id_length = i32::try_from(chunk.id.len()).map_err(|_| ArrowError::Limit)?;
    let mut header = [0; 20];
    header[..8].copy_from_slice(&length.to_be_bytes());
    header[8..12].copy_from_slice(&chunk.partition.to_be_bytes());
    header[12..16].copy_from_slice(&chunk.ordinal.to_be_bytes());
    header[16..20].copy_from_slice(&id_length.to_be_bytes());
    for bytes in [
        header.as_slice(),
        chunk.id.as_bytes(),
        chunk.payload.as_slice(),
    ] {
        writer.write_all(bytes).map_err(|_| ArrowError::Output)?;
    }
    Ok(())
}

/// End an explicitly labeled raw framed-v2 output after confirmed successful EOF.
pub fn write_terminal(writer: &mut impl Write) -> Result<(), ArrowError> {
    writer.write_all(&0_i64.to_be_bytes()).map_err(|_| ArrowError::Output)
}
