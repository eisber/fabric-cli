// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_cli_output::CommandError;

/// Every registered Python job leaf, without introducing aliases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobKind {
    /// Submit asynchronously.
    Start,
    /// Submit and wait for execution.
    Run,
    /// List instances or schedules.
    List,
    /// Cancel an instance, optionally waiting.
    Cancel,
    /// Get instance or schedule details.
    Status,
    /// Create a schedule.
    Schedule,
    /// Update an existing schedule.
    Update,
    /// Remove a schedule after confirmation.
    Remove,
}

impl JobKind {
    /// Canonical Python leaf spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Run => "run",
            Self::List => "run-list",
            Self::Cancel => "run-cancel",
            Self::Status => "run-status",
            Self::Schedule => "run-sch",
            Self::Update => "run-update",
            Self::Remove => "run-rm",
        }
    }
}

/// Source arguments, preserving token cardinality and absent versus supplied input.
#[derive(Clone, Debug)]
pub struct JobCommand {
    /// Selected leaf.
    pub kind: JobKind,
    /// Path tokens joined by the existing context resolver.
    pub path: Vec<String>,
    /// `--id` for an instance or schedule.
    pub id: Option<String>,
    /// Parameters in Python's `name:type=value` format.
    pub params: Option<Vec<String>>,
    /// Notebook configuration, inline or a caller-resolved file.
    pub config: Option<Vec<String>>,
    /// Execution or schedule JSON, inline or a caller-resolved file.
    pub input: Option<Vec<String>>,
    /// Signed Python integer; zero and negative timeouts are not parser errors.
    pub timeout: Option<i64>,
    /// Positive custom interval for synchronous run only.
    pub polling_interval: Option<u64>,
    /// Select schedules instead of instances.
    pub schedule: bool,
    /// Wait for cancellation to reach a terminal job state.
    pub wait: bool,
    /// Skip schedule-removal confirmation.
    pub force: bool,
    /// Source schedule switches and fields.
    pub schedule_args: ScheduleArgs,
    /// Optional explicit output selection; otherwise the host supplies it.
    pub output_format: Option<String>,
    /// Hidden source skill header selection; requires coordinated host header plumbing.
    pub skill: Option<String>,
}

impl JobCommand {
    /// Canonical leaf path used by source decorated errors and synchronous completion.
    pub fn command_path(&self) -> String {
        format!("job {}", self.kind.as_str())
    }
}

/// Raw schedule switches; `disable` is represented by whether the flag occurred.
#[derive(Clone, Debug, Default)]
pub struct ScheduleArgs {
    /// Enable the schedule.
    pub enable: bool,
    /// `--disable` occurred (Python stores `False` in `args.disable`).
    pub disable: bool,
    /// Lowercase `cron`, `daily`, or `weekly`.
    pub schedule_type: Option<String>,
    /// Minutes or comma-separated times.
    pub interval: Option<String>,
    /// Start timestamp, source syntax validation only.
    pub start: Option<String>,
    /// End timestamp, source syntax validation only.
    pub end: Option<String>,
    /// Case-sensitive comma-separated weekday names.
    pub days: Option<String>,
}

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}

fn tokens(name: &'static str, short: char) -> Arg {
    Arg::new(name)
        .long(name)
        .short(short)
        .num_args(1..)
        .action(ArgAction::Set)
}

/// Compose this registration into the single front door with `.subcommand(...)`.
///
/// No runtime, credentials, global configuration, or service call is installed.
pub fn job_subcommand() -> Command {
    let mut group = Command::new("job")
        .about("Run and schedule Fabric item jobs")
        .args_override_self(true)
        .infer_long_args(true)
        .arg_required_else_help(true);
    for kind in [
        JobKind::Start,
        JobKind::Run,
        JobKind::List,
        JobKind::Cancel,
        JobKind::Status,
        JobKind::Schedule,
        JobKind::Update,
        JobKind::Remove,
    ] {
        let mut leaf = Command::new(kind.as_str())
            .args_override_self(true)
            .infer_long_args(true)
            .arg(Arg::new("path").required(true).num_args(1..))
            .arg(
                Arg::new("output_format")
                    .long("output_format")
                    .value_parser(["json", "text"]),
            )
            .arg(Arg::new("skill").long("skill").hide(true));
        if matches!(kind, JobKind::Start | JobKind::Run) {
            leaf = leaf
                .arg(tokens("params", 'P'))
                .arg(tokens("config", 'C'))
                .arg(tokens("input", 'i'));
        }
        if kind == JobKind::Run {
            leaf = leaf
                .arg(
                    Arg::new("timeout")
                        .long("timeout")
                        .allow_negative_numbers(true)
                        .value_parser(source_timeout),
                )
                .arg(
                    Arg::new("polling_interval")
                        .long("polling_interval")
                        .value_parser(source_positive_interval),
                );
        }
        if matches!(
            kind,
            JobKind::Cancel | JobKind::Status | JobKind::Update | JobKind::Remove
        ) {
            leaf = leaf.arg(Arg::new("id").long("id").required(true));
        }
        if matches!(kind, JobKind::List | JobKind::Status) {
            leaf = leaf.arg(flag("schedule"));
        }
        if kind == JobKind::Cancel {
            leaf = leaf.arg(flag("wait").short('w'));
        }
        if kind == JobKind::Remove {
            leaf = leaf.arg(flag("force").short('f'));
        }
        if matches!(kind, JobKind::Schedule | JobKind::Update) {
            leaf = leaf.arg(tokens("input", 'i')).arg(flag("enable")).arg(
                Arg::new("type")
                    .long("type")
                    .value_parser(["cron", "daily", "weekly"]),
            );
            for name in ["interval", "start", "end", "days"] {
                leaf = leaf.arg(Arg::new(name).long(name));
            }
        }
        if kind == JobKind::Update {
            leaf = leaf.arg(flag("disable"));
        }
        group = group.subcommand(leaf);
    }
    group
}

/// Convert matches from `job_subcommand` into typed handler input.
///
/// Pass the matches for the `job` group, not the root parser.
pub fn command_from_matches(group: &ArgMatches) -> Result<JobCommand, CommandError> {
    let (name, args) = group
        .subcommand()
        .ok_or_else(|| invalid("A job subcommand is required"))?;
    let kind = match name {
        "start" => JobKind::Start,
        "run" => JobKind::Run,
        "run-list" => JobKind::List,
        "run-cancel" => JobKind::Cancel,
        "run-status" => JobKind::Status,
        "run-sch" => JobKind::Schedule,
        "run-update" => JobKind::Update,
        "run-rm" => JobKind::Remove,
        _ => return Err(invalid("Unknown job subcommand")),
    };
    let string = |name: &str| args.try_get_one::<String>(name).ok().flatten().cloned();
    let values = |name: &str| {
        args.try_get_many::<String>(name)
            .ok()
            .flatten()
            .map(|values| values.cloned().collect())
    };
    let boolean = |name: &str| {
        args.try_get_one::<bool>(name)
            .ok()
            .flatten()
            .copied()
            .unwrap_or(false)
    };
    Ok(JobCommand {
        kind,
        path: values("path").ok_or_else(|| invalid("A job path is required"))?,
        id: string("id"),
        params: values("params"),
        config: values("config"),
        input: values("input"),
        timeout: args.try_get_one::<i64>("timeout").ok().flatten().copied(),
        polling_interval: args
            .try_get_one::<u64>("polling_interval")
            .ok()
            .flatten()
            .copied(),
        schedule: boolean("schedule"),
        wait: boolean("wait"),
        force: boolean("force"),
        schedule_args: ScheduleArgs {
            enable: boolean("enable"),
            disable: boolean("disable"),
            schedule_type: string("type"),
            interval: string("interval"),
            start: string("start"),
            end: string("end"),
            days: string("days"),
        },
        output_format: string("output_format"),
        skill: string("skill"),
    })
}

pub(crate) fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(Some(message.into()), Some("InvalidInput".into()))
}

fn source_timeout(value: &str) -> Result<i64, String> {
    fabric_sdk_jobs::source_integer_text(value)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| "Timeout must be a supported signed integer".into())
}

fn source_positive_interval(value: &str) -> Result<u64, String> {
    fabric_sdk_jobs::source_integer_text(value)
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "Polling interval must be a supported positive integer".into())
}
