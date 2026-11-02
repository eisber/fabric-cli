// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic I/O injection into the actual production parser and job session.

#[allow(dead_code)]
mod support {
    include!("../tests/job_execution_support/mod.rs");
}

use std::io::{self, BufRead};

fn main() {
    for line in io::stdin().lock().lines() {
        let input = serde_json::from_str(&line.expect("stdin")).expect("case JSON");
        println!("{}", support::observe(&input));
    }
}
