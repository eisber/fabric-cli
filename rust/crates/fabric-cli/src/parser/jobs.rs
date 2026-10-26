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
        .about("Read Fabric item job runs and schedules")
        .arg_required_else_help(true)
        .subcommands(
            source
                .get_subcommands()
                .filter(|leaf| matches!(leaf.get_name(), "run-list" | "run-status"))
                .cloned(),
        )
}

pub(super) fn parse(group: &ArgMatches) -> Result<JobReadCommand, clap::Error> {
    let command =
        fabric_cli_jobs::command_from_matches(group).map_err(|_| super::invalid_arguments())?;
    let kind = match command.kind {
        JobKind::List => JobReadKind::List,
        JobKind::Status => JobReadKind::Status,
        _ => return Err(super::invalid_arguments()),
    };
    Ok(JobReadCommand {
        kind,
        path: command.path,
        id: command.id,
        schedule: command.schedule,
        output_format: command.output_format,
        skill: command.skill,
    })
}
