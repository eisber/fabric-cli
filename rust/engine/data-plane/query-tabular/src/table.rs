// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use arrow_array::{ArrayRef, RecordBatch, RecordBatchOptions};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use fabric_sdk_tabular::TabularResult;

use crate::{ConversionControl, ConversionError, ConversionLimit, ConversionLimits};

pub(crate) struct TableBudget {
    limits: ConversionLimits,
    rows: usize,
    batches: usize,
    bytes: usize,
}

impl TableBudget {
    pub(crate) fn new(limits: ConversionLimits) -> Self {
        Self {
            limits,
            rows: 0,
            batches: 0,
            bytes: 0,
        }
    }

    fn charge(&mut self, bytes: usize) -> Result<(), ConversionError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|value| *value <= self.limits.bytes)
            .ok_or(ConversionError::Limit(ConversionLimit::Bytes))?;
        Ok(())
    }

    pub(crate) fn table(
        &mut self,
        schema: SchemaRef,
        arrays: Vec<ArrayRef>,
        rows: usize,
        control: &ConversionControl<'_>,
    ) -> Result<TabularResult, ConversionError> {
        control.check()?;
        self.rows = self
            .rows
            .checked_add(rows)
            .filter(|value| *value <= self.limits.rows)
            .ok_or(ConversionError::Limit(ConversionLimit::Rows))?;
        self.batches = self
            .batches
            .checked_add(1)
            .filter(|value| *value <= self.limits.batches)
            .ok_or(ConversionError::Limit(ConversionLimit::Batches))?;
        if schema.fields().len() > self.limits.columns {
            return Err(ConversionError::Limit(ConversionLimit::Columns));
        }
        self.charge(std::mem::size_of::<Schema>())?;
        for field in schema.fields() {
            control.check()?;
            self.charge(std::mem::size_of::<Field>())?;
            self.charge(field.name().len())?;
            if let DataType::Timestamp(_, Some(timezone)) = field.data_type() {
                self.charge(timezone.len())?;
            }
            for (key, value) in field.metadata() {
                self.charge(2 * std::mem::size_of::<String>())?;
                self.charge(key.len())?;
                self.charge(value.len())?;
            }
        }
        for (key, value) in schema.metadata() {
            self.charge(2 * std::mem::size_of::<String>())?;
            self.charge(key.len())?;
            self.charge(value.len())?;
        }
        for array in &arrays {
            control.check()?;
            self.charge(array.get_array_memory_size())?;
        }
        let batch = RecordBatch::try_new_with_options(
            schema.clone(),
            arrays,
            &RecordBatchOptions::new().with_row_count(Some(rows)),
        )
        .map_err(ConversionError::Arrow)?;
        let table = TabularResult::try_new(schema, vec![batch], self.limits.output()?)?;
        control.check()?;
        Ok(table)
    }
}
