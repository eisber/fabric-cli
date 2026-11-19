// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit offline Fabric documentation bundle builder.

#![cfg_attr(test, deny(unnameable_test_items))]

use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Arg, Command};

struct Arguments {
    source: PathBuf,
    output: PathBuf,
    revision: String,
    compression: String,
    profile: String,
}

fn main() -> ExitCode {
    let matches = Command::new("build-docs-index")
        .about("Build a pinned, offline Fabric documentation index")
        .arg(
            Arg::new("source")
                .long("source")
                .value_name("ABS")
                .required(true)
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("output")
                .long("output")
                .value_name("NEW_ABS")
                .required(true)
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("revision")
                .long("revision")
                .value_name("SHA")
                .required(true),
        )
        .arg(
            Arg::new("compression")
                .long("compression")
                .value_parser(["lz4", "zstd", "none"])
                .default_value("lz4"),
        )
        .arg(
            Arg::new("profile")
                .long("profile")
                .help("Lexical index additions; profiles preserve corpus and sectioning")
                .value_parser([
                    "baseline",
                    "stemmed",
                    "metadata",
                    "hierarchy",
                    "autocomplete",
                    "full",
                ])
                .default_value("full"),
        )
        .get_matches();
    let arguments = Arguments {
        source: matches
            .get_one::<PathBuf>("source")
            .expect("required source")
            .clone(),
        output: matches
            .get_one::<PathBuf>("output")
            .expect("required output")
            .clone(),
        revision: matches
            .get_one::<String>("revision")
            .expect("required revision")
            .clone(),
        compression: matches
            .get_one::<String>("compression")
            .expect("default compression")
            .clone(),
        profile: matches
            .get_one::<String>("profile")
            .expect("default profile")
            .clone(),
    };
    match run(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("build-docs-index: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Arguments) -> Result<(), Box<dyn std::error::Error>> {
    let features = fabric_docs_search::rich::IndexFeatures::profile(&arguments.profile)
        .ok_or("unknown lexical index profile")?;
    let manifest = fabric_docs_search::builder::build_with_features(
        &arguments.source,
        &arguments.output,
        &arguments.revision,
        &arguments.compression,
        features,
    )?;
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &manifest)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
