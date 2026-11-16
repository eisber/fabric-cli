// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Generate synthetic physical inputs for actual-binary offline argv checks.
//! Supply an existing empty directory. No credentials or service are accessed.

use arrow_array::{Float64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use std::{error::Error, fs::OpenOptions, io::Write, path::PathBuf, sync::Arc};

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Expected one existing empty fixture directory")?,
    );
    if !root.is_dir() {
        return Err("Fixture directory does not exist".into());
    }
    for (name, bytes) in [
        ("record.json", b"[{\"x\":0}]".as_slice()),
        ("record.csv", b"x\r\n0\r\n".as_slice()),
    ] {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(name))?
            .write_all(bytes)?;
    }
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Float64, false)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Float64Array::from(vec![0.0]))],
    )?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("record.parquet"))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    println!("Generated synthetic JSON/CSV/Parquet record fixtures; no inference performed.");
    Ok(())
}
