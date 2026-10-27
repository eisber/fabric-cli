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

/// Remaining source job leaves, distinct from submission and the read-only DTO.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobMutationKind {
    /// Cancel an existing instance.
    Cancel,
    /// Create a schedule.
    Schedule,
    /// Update an existing schedule.
    Update,
    /// Remove a schedule.
    Remove,
}

/// Source arguments for cancellation and schedule changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobMutationCommand {
    /// Canonical mutation leaf.
    pub kind: JobMutationKind,
    /// Original path tokens.
    pub path: Vec<String>,
    /// Required instance/schedule ID, absent for creation.
    pub id: Option<String>,
    /// Schedule JSON, inline or an explicitly selected file.
    pub input: Option<Vec<String>>,
    /// Wait for actual cancellation status.
    pub wait: bool,
    /// Explicitly bypass removal confirmation.
    pub force: bool,
    /// Enable the schedule.
    pub enable: bool,
    /// Source disable flag occurrence, not its inverted stored value.
    pub disable: bool,
    /// Schedule type.
    pub schedule_type: Option<String>,
    /// Interval or source time list.
    pub interval: Option<String>,
    /// Source start timestamp.
    pub start: Option<String>,
    /// Source end timestamp.
    pub end: Option<String>,
    /// Source weekday list.
    pub days: Option<String>,
    /// Explicit source output format.
    pub output_format: Option<String>,
    /// Explicit source header selection.
    pub skill: Option<String>,
}

impl JobMutationCommand {
    /// Pass source values to the released handler without interpreting its payload.
    pub fn family_command(&self) -> JobCommand {
        JobCommand {
            kind: match self.kind {
                JobMutationKind::Cancel => JobKind::Cancel,
                JobMutationKind::Schedule => JobKind::Schedule,
                JobMutationKind::Update => JobKind::Update,
                JobMutationKind::Remove => JobKind::Remove,
            },
            path: self.path.clone(),
            id: self.id.clone(),
            input: self.input.clone(),
            wait: self.wait,
            force: self.force,
            schedule_args: ScheduleArgs {
                enable: self.enable,
                disable: self.disable,
                schedule_type: self.schedule_type.clone(),
                interval: self.interval.clone(),
                start: self.start.clone(),
                end: self.end.clone(),
                days: self.days.clone(),
            },
            output_format: self.output_format.clone(),
            skill: self.skill.clone(),
            params: None,
            config: None,
            timeout: None,
            polling_interval: None,
            schedule: false,
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
    fabric_cli_jobs::job_subcommand()
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
    let mutation = match command.kind {
        JobKind::Cancel => Some(JobMutationKind::Cancel),
        JobKind::Schedule => Some(JobMutationKind::Schedule),
        JobKind::Update => Some(JobMutationKind::Update),
        JobKind::Remove => Some(JobMutationKind::Remove),
        _ => None,
    };
    if let Some(kind) = mutation {
        return Ok(super::Invocation::JobMutation(JobMutationCommand {
            kind,
            path: command.path,
            id: command.id,
            input: command.input,
            wait: command.wait,
            force: command.force,
            enable: command.schedule_args.enable,
            disable: command.schedule_args.disable,
            schedule_type: command.schedule_args.schedule_type,
            interval: command.schedule_args.interval,
            start: command.schedule_args.start,
            end: command.schedule_args.end,
            days: command.schedule_args.days,
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
