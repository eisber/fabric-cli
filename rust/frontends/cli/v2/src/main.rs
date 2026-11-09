// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Outer-only composition for the additive six-leaf local CLI head.

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use fabric_cli_v2::{
    CliError, Control, auth, configure, output,
    parser::{self, Operation, Parsed},
    report, show, upgrade, validate,
};

struct Deadline {
    started: Instant,
    timeout: Duration,
}

impl Control for Deadline {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn deadline_elapsed(&self) -> bool {
        self.started.elapsed() >= self.timeout
    }
}

fn absolute_root(path: PathBuf) -> Result<PathBuf, CliError> {
    if !path.is_absolute() {
        return Err(CliError::new(
            "InvalidInput",
            "The v2 preference root must be an absolute directory.",
        ));
    }
    Ok(path)
}

fn optional_preference_root(explicit: Option<&Path>) -> Result<Option<PathBuf>, CliError> {
    if let Some(root) = explicit {
        return absolute_root(root.to_owned()).map(Some);
    }
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Microsoft").join("FabricCliV2"));
    #[cfg(not(windows))]
    let root = if let Some(root) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(root).join("microsoft").join("fabric-cli-v2"))
    } else {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("microsoft")
                .join("fabric-cli-v2")
        })
    };
    root.map(absolute_root).transpose()
}

fn preference_root(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    optional_preference_root(explicit)?.ok_or_else(|| {
        CliError::new(
            "MissingConfiguration",
            "Provide --config-root with an absolute isolated preference directory.",
        )
    })
}

fn run() -> u8 {
    let invocation = match parser::parse_args(std::env::args_os()) {
        Ok(Parsed::Display(text)) => {
            return if write!(io::stdout().lock(), "{text}").is_ok() {
                0
            } else {
                let _diagnostic =
                    writeln!(io::stderr().lock(), "fab-cli: writing help/version failed.");
                1
            };
        }
        Ok(Parsed::Execute(invocation)) => invocation,
        Err(_) => {
            let failure = CliError::new(
                "InvalidInput",
                "Invalid arguments. Run fab-cli --help for the selected commands and usage.",
            );
            return output::write_outcome(
                "cli",
                Err(failure),
                parser::Format::Json,
                io::stdout().lock(),
                io::stderr().lock(),
            )
            .unwrap_or(1);
        }
    };
    let mut installation = upgrade::UpgradeSession::new();
    let name = invocation.operation.name();
    let timeout = match &invocation.operation {
        Operation::Show {
            timeout_seconds, ..
        } => *timeout_seconds,
        _ => 30,
    };
    let control = Deadline {
        started: Instant::now(),
        timeout: Duration::from_secs(timeout),
    };
    let mut format = invocation.format;
    let result = (|| {
        let unavailable = (matches!(&invocation.operation, Operation::Auth { .. })
            && !cfg!(all(windows, feature = "windows-broker")))
            || (matches!(&invocation.operation, Operation::Upgrade(_))
                && !cfg!(feature = "self-update"));
        let needs_preferences = !matches!(
            &invocation.operation,
            Operation::Configure { .. } | Operation::Validate(_)
        ) && !unavailable
            && (!invocation.format_explicit
                || matches!(
                    &invocation.operation,
                    Operation::Show {
                        experience_explicit: false,
                        ..
                    }
                ));
        let settings = if needs_preferences {
            optional_preference_root(invocation.config_root.as_deref())?
                .map(|root| configure::read(&root))
                .transpose()?
                .unwrap_or_default()
        } else {
            serde_json::Map::new()
        };
        if !invocation.format_explicit && needs_preferences {
            format = match settings
                .get("output_format")
                .and_then(serde_json::Value::as_str)
            {
                Some("text") => parser::Format::Text,
                Some("json") | None => parser::Format::Json,
                _ => {
                    return Err(CliError::new(
                        "InvalidConfiguration",
                        "Invalid output preference.",
                    ));
                }
            };
        }
        match &invocation.operation {
            Operation::Validate(request) => validate::run(request, &control),
            Operation::Configure { target, value } => {
                preference_root(invocation.config_root.as_deref())
                    .and_then(|root| configure::set(&root, target, value, &control))
            }
            Operation::Auth { action, request } => auth::run(*action, request),
            Operation::Show {
                path,
                experience,
                experience_explicit,
                uri_only,
                ..
            } => show::run(
                path,
                if *experience_explicit {
                    experience
                } else {
                    settings
                        .get("default_open_experience")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(experience)
                },
                *uri_only,
                &mut show::SystemViewer,
                &control,
            ),
            Operation::Report(request) => report::prepare(request),
            Operation::Upgrade(request) => installation.run(request),
        }
    })();
    let invalid_document = matches!(&invocation.operation, Operation::Validate(_))
        && result
            .as_ref()
            .is_ok_and(|report| report.data["valid"] == false);
    match output::write_outcome(
        name,
        result,
        format,
        io::stdout().lock(),
        io::stderr().lock(),
    ) {
        Ok(0) if invalid_document => 1,
        Ok(exit) => exit,
        Err(_) => {
            let _diagnostic = writeln!(
                io::stderr().lock(),
                "fab-cli: result output failed; effects were not repeated."
            );
            1
        }
    }
}

fn main() -> ExitCode {
    ExitCode::from(run())
}
