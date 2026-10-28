// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure selection for the binary's explicitly selected local settings and deadline.

use fabric_cli::{ItemSort, ReadConfig, parser::ReadCommand};
use fabric_cli_jobs::{JobCommand, JobConfig, JobKind};
use fabric_cli_local::SETTINGS;
use fabric_cli_output::text::OutputFormat;
use serde_json::{Map, Value};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SelectionError {
    #[error("Invalid saved native setting: {0}.")]
    InvalidSetting(&'static str),
    #[error("Invalid native output selection.")]
    InvalidOutput,
    #[error("The selected job timeout exceeds the native deadline range.")]
    DeadlineRange,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeSettings {
    pub(crate) reads: ReadConfig,
    pub(crate) jobs: JobConfig,
    output: &'static str,
}

impl NativeSettings {
    pub(crate) fn select(saved: Option<&Map<String, Value>>) -> Result<Self, SelectionError> {
        let Some(saved) = saved else {
            // No store is a supported preview flow, not an implicit user profile.
            return Ok(Self {
                reads: ReadConfig::default(),
                jobs: JobConfig::default(),
                output: "json",
            });
        };
        Ok(Self {
            reads: ReadConfig {
                show_hidden: setting(saved, "show_hidden")? == "true",
                item_sort: match setting(saved, "output_item_sort_criteria")? {
                    "byname" => ItemSort::ByName,
                    "bytype" => ItemSort::ByType,
                    _ => return Err(SelectionError::InvalidSetting("output_item_sort_criteria")),
                },
                folder_listing_enabled: setting(saved, "folder_listing_enabled")? == "true",
            },
            jobs: JobConfig {
                cancel_on_timeout: setting(saved, "job_cancel_ontimeout")? == "true",
            },
            output: match setting(saved, "output_format")? {
                "text" => "text",
                "json" => "json",
                _ => return Err(SelectionError::InvalidSetting("output_format")),
            },
        })
    }

    pub(crate) fn output(&self, explicit: Option<&str>) -> Result<String, SelectionError> {
        match OutputFormat::resolve(explicit, Some(self.output))
            .map_err(|_| SelectionError::InvalidOutput)?
        {
            OutputFormat::Json => Ok("json".into()),
            OutputFormat::Text => Ok("text".into()),
        }
    }

    pub(crate) fn read_command(
        &self,
        command: &ReadCommand,
    ) -> Result<ReadCommand, SelectionError> {
        let mut selected = command.clone();
        selected.output_format = Some(self.output(command.output_format.as_deref())?);
        Ok(selected)
    }

    pub(crate) fn job_command(&self, command: &JobCommand) -> Result<JobCommand, SelectionError> {
        let mut selected = command.clone();
        selected.output_format = Some(self.output(command.output_format.as_deref())?);
        Ok(selected)
    }
}

fn setting<'a>(
    saved: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, SelectionError> {
    let schema = SETTINGS
        .iter()
        .find(|setting| setting.key == key)
        .expect("existing setting");
    let value = match saved.get(key) {
        Some(value) => value.as_str().ok_or(SelectionError::InvalidSetting(key))?,
        None => schema
            .default
            .expect("consumed settings have source defaults"),
    };
    if schema.allowed.contains(&value) {
        Ok(value)
    } else {
        Err(SelectionError::InvalidSetting(key))
    }
}

/// Keep the preview cap except for synchronous runs with an explicit finite timeout.
///
/// The family still owns its timeout, polling and remote cancellation. This is
/// only a last-resort wall-clock watchdog, including bounded cleanup headroom.
pub(crate) fn command_deadline(
    job: Option<&JobCommand>,
    config: JobConfig,
) -> Result<Duration, SelectionError> {
    let http = fabric_sdk_http::ExecutionOptions::default();
    let Some(command) = job.filter(|command| command.kind == JobKind::Run) else {
        return Ok(http.timeout);
    };
    let Some(timeout) = command.timeout.filter(|timeout| *timeout >= 0) else {
        // Python has no implicit job timeout. Do not invent one, or change the
        // family's negative-sleep error into timeout cancellation.
        return Ok(http.timeout);
    };
    let timeout =
        Duration::from_secs(u64::try_from(timeout).map_err(|_| SelectionError::DeadlineRange)?);
    // Python/the family count integer seconds per status request. A positive
    // one-second interval can lose nearly one more second on every iteration.
    let wait = timeout
        .checked_mul(2)
        .ok_or(SelectionError::DeadlineRange)?;
    let mut budget = http
        .timeout
        .checked_add(wait)
        .and_then(|budget| budget.checked_add(http.timeout))
        .and_then(|budget| budget.checked_add(http.max_delay))
        .ok_or(SelectionError::DeadlineRange)?;
    if config.cancel_on_timeout {
        let operations = fabric_sdk_operations::OperationLimits::default();
        let requests = u32::try_from(operations.max_requests.get())
            .map_err(|_| SelectionError::DeadlineRange)?;
        let cleanup = http
            .timeout
            .checked_add(operations.max_poll_delay)
            .and_then(|exchange| exchange.checked_mul(requests))
            .and_then(|reconciliation| reconciliation.checked_add(http.timeout))
            .ok_or(SelectionError::DeadlineRange)?;
        budget = budget
            .checked_add(cleanup)
            .ok_or(SelectionError::DeadlineRange)?;
    }
    Ok(budget)
}
