// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use fabric_sdk_tabular::OutputLimits;
use serde_json::Value;

use crate::{ConversionError, ConversionLimit};

/// Finite source and aggregate conversion budgets; no unlimited default exists.
///
/// Bytes bound source payload and retained Arrow storage independently, not
/// allocator-wide resident memory. Metadata, names, and values all count as
/// source payload. Shared codecs apply their own output budgets separately.
#[derive(Clone, Copy, Debug)]
pub struct ConversionLimits {
    pub(crate) datasets: usize,
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    pub(crate) bytes: usize,
    pub(crate) batches: usize,
    pub(crate) depth: usize,
}

pub(crate) fn json_text(
    value: &Value,
    max_bytes: usize,
    control: &ConversionControl<'_>,
) -> Result<String, ConversionError> {
    struct BoundedJson<'a, 'b> {
        bytes: Vec<u8>,
        max_bytes: usize,
        exceeded: bool,
        control: &'a ConversionControl<'b>,
        stopped: Option<ConversionError>,
    }
    impl Write for BoundedJson<'_, '_> {
        fn write(&mut self, value: &[u8]) -> io::Result<usize> {
            if let Err(error) = self.control.check() {
                self.stopped = Some(error);
                return Err(io::Error::other("logical JSON conversion interrupted"));
            }
            if self
                .bytes
                .len()
                .checked_add(value.len())
                .is_none_or(|size| size > self.max_bytes)
            {
                self.exceeded = true;
                return Err(io::Error::other("JSON logical value exceeds byte budget"));
            }
            self.bytes.extend_from_slice(value);
            Ok(value.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedJson {
        bytes: Vec::new(),
        max_bytes,
        exceeded: false,
        control,
        stopped: None,
    };
    if let Err(error) = serde_json::to_writer(&mut output, value) {
        if let Some(error) = output.stopped {
            return Err(error);
        }
        return Err(if output.exceeded {
            ConversionError::Limit(ConversionLimit::Bytes)
        } else {
            ConversionError::Json(error)
        });
    }
    control.check()?;
    // serde_json writes valid UTF-8; conversion does not replace invalid bytes.
    String::from_utf8(output.bytes).map_err(|_| ConversionError::LogicalEncoding)
}

impl ConversionLimits {
    /// Creates explicit limits. Zero rows permits typed empty results.
    ///
    /// Bytes cannot exceed `i32::MAX`, keeping UTF-8 Arrow offsets representable.
    /// Nesting is capped at 128 to bound the converter's validation stack.
    pub fn new(
        max_datasets: usize,
        max_rows: usize,
        max_columns: usize,
        max_bytes: usize,
        max_batches: usize,
        max_value_depth: usize,
    ) -> Result<Self, ConversionError> {
        if max_datasets == 0
            || max_columns == 0
            || max_bytes == 0
            || max_bytes > i32::MAX as usize
            || max_batches == 0
            || !(1..=128).contains(&max_value_depth)
        {
            return Err(ConversionError::InvalidLimits);
        }
        Ok(Self {
            datasets: max_datasets,
            rows: max_rows,
            columns: max_columns,
            bytes: max_bytes,
            batches: max_batches,
            depth: max_value_depth,
        })
    }

    pub(crate) fn output(self) -> Result<OutputLimits, ConversionError> {
        Ok(OutputLimits::new(self.rows, self.bytes, self.batches)?)
    }
}

/// Caller-owned cancellation and absolute deadline, checked throughout conversion.
pub struct ConversionControl<'a> {
    deadline: Instant,
    cancelled: &'a AtomicBool,
}

impl<'a> ConversionControl<'a> {
    /// Borrows a cancellation flag and an already chosen absolute deadline.
    pub fn new(deadline: Instant, cancelled: &'a AtomicBool) -> Self {
        Self {
            deadline,
            cancelled,
        }
    }

    pub(crate) fn check(&self) -> Result<(), ConversionError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(ConversionError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(ConversionError::Deadline);
        }
        Ok(())
    }
}

pub(crate) struct Budget<'a, 'b> {
    limits: ConversionLimits,
    control: &'a ConversionControl<'b>,
    bytes: usize,
}

impl<'a, 'b> Budget<'a, 'b> {
    pub(crate) fn new(limits: ConversionLimits, control: &'a ConversionControl<'b>) -> Self {
        Self {
            limits,
            control,
            bytes: 0,
        }
    }

    pub(crate) fn bytes(&mut self, amount: usize) -> Result<(), ConversionError> {
        self.control.check()?;
        self.bytes = self
            .bytes
            .checked_add(amount)
            .filter(|value| *value <= self.limits.bytes)
            .ok_or(ConversionError::Limit(ConversionLimit::Bytes))?;
        Ok(())
    }

    pub(crate) fn value(&mut self, value: &Value, depth: usize) -> Result<(), ConversionError> {
        self.control.check()?;
        if depth > self.limits.depth {
            return Err(ConversionError::Limit(ConversionLimit::ValueDepth));
        }
        // Include container/cell overhead even for empty arrays, objects and nulls.
        self.bytes(std::mem::size_of::<Value>())?;
        match value {
            Value::String(value) => self.bytes(value.len()),
            Value::Number(value) => self.bytes(value.to_string().len()),
            Value::Array(values) => {
                for value in values {
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Object(values) => {
                for (key, value) in values {
                    self.bytes(key.len())?;
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Bool(_) | Value::Null => Ok(()),
        }
    }
}
