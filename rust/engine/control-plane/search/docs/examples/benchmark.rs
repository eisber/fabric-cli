// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded offline same-reader measurements, not user traffic or performance promises.
//!
//! Run a separately prepared release executable with `--iterations 100`, optionally
//! `--copy` (the same embedded components copied into RAM) and `--queries ABSOLUTE_JSON`.
//! The fixture documents schema 1. Reader open is measured once, each case has one
//! unaggregated warmup, and measured samples exclude open and JSON serialization.
//! Stdout is exactly one JSON report, including on failure. Relevance misses are
//! evidence, not silently replaced expectations, and produce exit status 1.

use fabric_docs_search::{DocsSearch, Hit, SearchOptions, Timings};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    time::Instant,
};

const SOURCE_COMMIT: &str = "3ae7afc73169ac57b874d5aee09217bb35a4f8fc";
const FIXTURE: &str = include_str!("../tests/fixtures/queries.json");
const MAX_FIXTURE_BYTES: u64 = 131_072;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    copyright: String,
    schema_version: u32,
    description: String,
    schema: String,
    source_commit: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    text: String,
    limit: usize,
    workload: Option<String>,
    fuzzy: u8,
    expected: Expected,
    grounding: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    kind: String,
    value: Option<String>,
}

struct Arguments {
    iterations: usize,
    copy: bool,
    queries: Option<PathBuf>,
}

impl Case {
    fn options(&self) -> SearchOptions {
        SearchOptions {
            text: self.text.clone(),
            limit: self.limit,
            workload: self.workload.clone(),
            fuzzy: self.fuzzy,
        }
    }

    fn matches(&self, hits: &[Hit]) -> bool {
        match (self.expected.kind.as_str(), self.expected.value.as_deref()) {
            ("no_hits", None) => hits.is_empty(),
            ("source_path", Some(path)) => hits.iter().any(|hit| hit.source_path == path),
            ("source_prefix", Some(prefix)) => {
                hits.iter().any(|hit| hit.source_path.starts_with(prefix))
            }
            _ => false,
        }
    }
}

fn arguments() -> Result<Arguments, String> {
    let mut result = Arguments {
        iterations: 100,
        copy: false,
        queries: None,
    };
    let mut seen = HashSet::new();
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        if !seen.insert(flag.clone()) {
            return Err("Duplicate benchmark option".into());
        }
        match flag.as_str() {
            "--iterations" => {
                let value = args.next().ok_or("Missing iteration count")?;
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("Iterations must be an integer from 1 through 1000".into());
                }
                result.iterations = value.parse().map_err(|_| "Invalid iteration count")?;
                if !(1..=1000).contains(&result.iterations) {
                    return Err("Iterations must be from 1 through 1000".into());
                }
            }
            "--copy" => result.copy = true,
            "--queries" => {
                let path = PathBuf::from(args.next().ok_or("Missing fixture path")?);
                if !path.is_absolute() || !path.is_file() {
                    return Err("Queries must name an absolute existing JSON file".into());
                }
                result.queries = Some(path);
            }
            _ => {
                return Err(
                    "Supported options: --iterations 1..1000 --copy --queries ABSOLUTE_JSON".into(),
                );
            }
        }
    }
    Ok(result)
}

fn fixture(args: &Arguments) -> Result<Fixture, String> {
    let bytes = match &args.queries {
        Some(path) => {
            let file = fs::File::open(path).map_err(|_| "Cannot open queries file")?;
            let mut bytes = Vec::new();
            // Read no more than the fixture budget, even if the file grows.
            io::Read::read_to_end(&mut io::Read::take(file, MAX_FIXTURE_BYTES + 1), &mut bytes)
                .map_err(|_| "Cannot read queries file")?;
            bytes
        }
        None => FIXTURE.as_bytes().to_vec(),
    };
    if bytes.len() as u64 > MAX_FIXTURE_BYTES {
        return Err("Queries exceed the fixture byte budget".into());
    }
    let fixture: Fixture =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid queries JSON/schema")?;
    if fixture.schema_version != 1
        || fixture.source_commit != SOURCE_COMMIT
        || fixture.copyright.trim().is_empty()
        || fixture.description.trim().is_empty()
        || fixture.schema.trim().is_empty()
        || !(1..=32).contains(&fixture.cases.len())
    {
        return Err("Unsupported queries schema, source pin, or case count".into());
    }
    let mut ids = HashSet::new();
    for case in &fixture.cases {
        if case.id.is_empty()
            || case.id.len() > 80
            || !case
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !ids.insert(&case.id)
            || case.grounding.trim().is_empty()
            || case.options().validate().is_err()
        {
            return Err("Invalid, duplicate, or ungrounded query case".into());
        }
        let valid_expectation = match (case.expected.kind.as_str(), case.expected.value.as_deref())
        {
            ("no_hits", None) => true,
            ("source_path", Some(value)) => source_path(value),
            ("source_prefix", Some(value)) => {
                value.len() > 5
                    && value.ends_with('/')
                    && source_path(&format!("{value}article.md"))
            }
            _ => false,
        };
        if !valid_expectation {
            return Err("Invalid source expectation".into());
        }
    }
    Ok(fixture)
}

fn source_path(value: &str) -> bool {
    value.starts_with("docs/")
        && (value.ends_with(".md") || value.ends_with(".yml") || value.ends_with(".yaml"))
        && !value.contains('\\')
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

fn distribution(mut values: Vec<u128>) -> Value {
    values.sort_unstable();
    let count = values.len();
    let middle = count / 2;
    let p50 = if count.is_multiple_of(2) {
        (values[middle - 1] as f64 + values[middle] as f64) / 2.0
    } else {
        values[middle] as f64
    };
    json!({
        "samples":count, "p50":p50, "p95":values[(95 * count).div_ceil(100) - 1],
        "min":values[0], "max":values[count - 1]
    })
}

fn sample(wall_us: u128, timings: &Timings, matched: bool, hits: &[Hit]) -> Value {
    json!({
        "wall_us":wall_us, "timings":timings, "expected_source_hit":matched,
        "observed_ids":hits.iter().map(|hit| &hit.id).collect::<Vec<_>>(),
        "observed_source_paths":hits.iter().map(|hit| &hit.source_path).collect::<Vec<_>>()
    })
}

fn measure(report: &mut Value) -> Result<bool, String> {
    let args = arguments()?;
    let fixture = fixture(&args)?;
    report["mode"] = json!(if args.copy { "copied_ram" } else { "embedded" });
    report["iterations"] = json!(args.iterations);
    report["source_commit"] = json!(fixture.source_commit);
    report["queries_path"] = json!(args.queries);
    let started = Instant::now();
    let reader = if args.copy {
        DocsSearch::copied_embedded()
    } else {
        DocsSearch::embedded()
    }
    .map_err(|error| error.to_string())?;
    report["open_us"] = json!(started.elapsed().as_micros());
    report["manifest"] =
        serde_json::to_value(reader.manifest()).map_err(|_| "Cannot encode manifest")?;
    if reader.manifest().source_commit != SOURCE_COMMIT {
        return Err("Embedded corpus does not match the query fixture source commit".into());
    }
    let mut all_matched = true;
    for case in &fixture.cases {
        let options = case.options();
        let mut row = json!({
            "id":case.id, "text":case.text, "limit":case.limit, "workload":case.workload,
            "fuzzy":case.fuzzy, "grounding":case.grounding,
            "expected":{"kind":case.expected.kind,"value":case.expected.value},
            "samples":[], "observed_top_hits":[]
        });
        let measured = (|| -> Result<(), String> {
            let started = Instant::now();
            let warmup = reader.search(&options).map_err(|error| error.to_string())?;
            let wall_us = started.elapsed().as_micros();
            let matched = case.matches(&warmup.data);
            all_matched &= matched;
            row["warmup"] = sample(wall_us, &warmup.timings, matched, &warmup.data);
            row["warmup_top_hits"] = json!(warmup.data);
            let mut walls = Vec::with_capacity(args.iterations);
            let mut parses = Vec::with_capacity(args.iterations);
            let mut searches = Vec::with_capacity(args.iterations);
            let mut fetches = Vec::with_capacity(args.iterations);
            let mut totals = Vec::with_capacity(args.iterations);
            for index in 0..args.iterations {
                let started = Instant::now();
                let result = reader.search(&options).map_err(|error| error.to_string())?;
                let wall_us = started.elapsed().as_micros();
                let matched = case.matches(&result.data);
                all_matched &= matched;
                walls.push(wall_us);
                parses.push(result.timings.parse_us);
                searches.push(result.timings.search_us);
                fetches.push(result.timings.fetch_us);
                totals.push(result.timings.total_us);
                if index == 0 {
                    row["observed_top_hits"] = json!(result.data);
                }
                row["samples"]
                    .as_array_mut()
                    .ok_or("Invalid sample container")?
                    .push(sample(wall_us, &result.timings, matched, &result.data));
            }
            row["distributions_us"] = json!({
                "wall":distribution(walls), "parse":distribution(parses),
                "search":distribution(searches), "fetch":distribution(fetches),
                "total":distribution(totals)
            });
            Ok(())
        })();
        if let Err(error) = &measured {
            row["error"] = json!(error);
        }
        report["cases"]
            .as_array_mut()
            .ok_or("Invalid case container")?
            .push(row);
        measured?;
    }
    Ok(all_matched)
}

fn main() -> ExitCode {
    let mut report = json!({
        "schema_version":1, "status":"failed", "cases":[],
        "cache_classification":"same-reader-warmed; OS-cache-uncontrolled",
        "units":"microseconds", "traffic_classification":"corpus-grounded-domain-probes-not-user-traffic",
        "percentiles":"p50 median; p95 nearest rank; no outliers removed"
    });
    let success = match measure(&mut report) {
        Ok(true) => {
            report["status"] = json!("passed");
            true
        }
        Ok(false) => {
            report["status"] = json!("failed_relevance");
            false
        }
        Err(error) => {
            report["error"] = json!(error);
            false
        }
    };
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if serde_json::to_writer(&mut output, &report).is_err()
        || writeln!(output).is_err()
        || output.flush().is_err()
    {
        return ExitCode::FAILURE;
    }
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
