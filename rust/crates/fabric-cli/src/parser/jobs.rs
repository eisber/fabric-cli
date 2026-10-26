// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{ArgMatches, Command};
use fabric_cli_jobs::{JobCommand, JobKind, ScheduleArgs};

/// The two read-only job leaves selected by the executable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobReadKind {
    /// List instances or schedules.
    List,
    /// Retrieve one instance or schedule.
    Status,
}

/// Source arguments for bounded job reads; no mutation can be selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobReadCommand {
    /// Read leaf.
    pub kind: JobReadKind,
    /// Source path tokens, without lossy shell reconstruction.
    pub path: Vec<String>,
    /// Required status ID; absent for lists.
    pub id: Option<String>,
    /// Select schedules instead of instances.
    pub schedule: bool,
    /// Explicit output selection; JSON is the native preview default.
    pub output_format: Option<String>,
    /// Hidden source skill selection, forwarded through the released job header API.
    pub skill: Option<String>,
}

/// Submission arguments passed to the existing jobs family without selecting I/O.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobRunCommand {
    /// Submit asynchronously when true; otherwise wait for the real job status.
    pub asynchronous: bool,
    /// Original source path tokens.
    pub path: Vec<String>,
    /// Source parameter tokens.
    pub params: Option<Vec<String>>,
    /// Notebook configuration tokens, inline or an explicit file.
    pub config: Option<Vec<String>>,
    /// Execution input tokens, inline or an explicit file.
    pub input: Option<Vec<String>>,
    /// Source signed timeout, including zero and negative values.
    pub timeout: Option<i64>,
    /// Positive custom status polling interval.
    pub polling_interval: Option<u64>,
    /// Explicit source output format.
    pub output_format: Option<String>,
    /// Explicit source request-header selection.
    pub skill: Option<String>,
}

impl JobRunCommand {
    /// Compose the released family DTO; no service algorithm is implemented here.
    pub fn family_command(&self) -> JobCommand {
        JobCommand {
            kind: if self.asynchronous {
                JobKind::Start
            } else {
                JobKind::Run
            },
            path: self.path.clone(),
            params: self.params.clone(),
            config: self.config.clone(),
            input: self.input.clone(),
            timeout: self.timeout,
            polling_interval: self.polling_interval,
            output_format: self.output_format.clone(),
            skill: self.skill.clone(),
            id: None,
            schedule: false,
            wait: false,
            force: false,
            schedule_args: ScheduleArgs::default(),
        }
    }
}

impl JobReadCommand {
    pub(crate) fn family_command(&self) -> JobCommand {
        JobCommand {
            kind: match self.kind {
                JobReadKind::List => JobKind::List,
                JobReadKind::Status => JobKind::Status,
            },
            path: self.path.clone(),
            id: self.id.clone(),
            schedule: self.schedule,
            output_format: self.output_format.clone(),
            skill: self.skill.clone(),
            params: None,
            config: None,
            input: None,
            timeout: None,
            polling_interval: None,
            wait: false,
            force: false,
            schedule_args: ScheduleArgs::default(),
        }
    }
}

pub(super) fn register() -> Command {
    let source = fabric_cli_jobs::job_subcommand();
    Command::new("job")
        .about("Run Fabric item jobs and read their runs and schedules")
        .arg_required_else_help(true)
        .subcommands(
            source
                .get_subcommands()
                .filter(|leaf| {
                    matches!(leaf.get_name(), "start" | "run" | "run-list" | "run-status")
                })
                .cloned(),
        )
}

pub(super) fn parse(group: &ArgMatches) -> Result<super::Invocation, clap::Error> {
    let command =
        fabric_cli_jobs::command_from_matches(group).map_err(|_| super::invalid_arguments())?;
    if matches!(command.kind, JobKind::Start | JobKind::Run) {
        return Ok(super::Invocation::JobRun(JobRunCommand {
            asynchronous: command.kind == JobKind::Start,
            path: command.path,
            params: command.params,
            config: command.config,
            input: command.input,
            timeout: command.timeout,
            polling_interval: command.polling_interval,
            output_format: command.output_format,
            skill: command.skill,
        }));
    }
    let kind = match command.kind {
        JobKind::List => JobReadKind::List,
        JobKind::Status => JobReadKind::Status,
        _ => return Err(super::invalid_arguments()),
    };
    Ok(super::Invocation::JobRead(JobReadCommand {
        kind,
        path: command.path,
        id: command.id,
        schedule: command.schedule,
        output_format: command.output_format,
        skill: command.skill,
    }))
}
