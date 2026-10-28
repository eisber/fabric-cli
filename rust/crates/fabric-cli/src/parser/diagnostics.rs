// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{
    Arg, Command,
    error::{ContextKind, ContextValue, ErrorKind},
};

/// Render invalid input using only registered argument labels, never argv values.
///
/// Raw Clap messages, usages, suggestions and rejected values may contain user
/// data. Only exact schema labels are projected from the error context.
pub fn invalid_arguments_message(error: &clap::Error) -> String {
    let description = match error.kind() {
        ErrorKind::MissingRequiredArgument => "Missing required argument(s)",
        ErrorKind::InvalidValue
            if matches!(
                error.get(ContextKind::InvalidValue),
                Some(ContextValue::String(value)) if value.is_empty()
            ) =>
        {
            "Missing value for argument"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => "Invalid value for argument",
        ErrorKind::TooFewValues | ErrorKind::WrongNumberOfValues => {
            "Incorrect number of values for argument"
        }
        _ => return super::INVALID_ARGUMENTS.into(),
    };
    let mut schema = super::command();
    schema.build();
    let mut labels = Vec::new();
    collect_labels(&schema, error.get(ContextKind::InvalidArg), &mut labels);
    if labels.is_empty() {
        return super::INVALID_ARGUMENTS.into();
    }
    format!(
        "{description}: {}. Run fab-rs --help for available commands and usage.",
        labels.join(", ")
    )
}

fn collect_labels(schema: &Command, context: Option<&ContextValue>, labels: &mut Vec<String>) {
    for argument in schema.get_arguments() {
        let label = argument.to_string();
        let matches = match context {
            Some(ContextValue::String(value)) => *value == label,
            Some(ContextValue::Strings(values)) => values.contains(&label),
            _ => false,
        };
        if matches && !labels.contains(&label) {
            labels.push(label);
        }
    }
    for child in schema.get_subcommands() {
        collect_labels(child, context, labels);
    }
}

pub(super) fn missing_value(argument: &Arg) -> clap::Error {
    let mut error = clap::Error::new(ErrorKind::InvalidValue);
    error.insert(
        ContextKind::InvalidArg,
        ContextValue::String(argument.to_string()),
    );
    error.insert(
        ContextKind::InvalidValue,
        ContextValue::String(String::new()),
    );
    error
}
